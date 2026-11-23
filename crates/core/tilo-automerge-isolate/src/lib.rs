//! Decode and apply untrusted Automerge bytes where a bad input can only cost a
//! worker process.
//!
//! Automerge is a CRDT library, not a parser hardened against hostile input:
//! malformed storage can make it panic, loop forever or allocate without bound, and
//! release builds abort on panic. So peer bytes pass [`precheck`] first, then the
//! same call runs in a worker process with its heap capped ([`Limits::memory_bytes`])
//! and a deadline per request ([`Limits::timeout`]), confined by Landlock and a
//! seccomp allow-list. Only input the worker itself refuses is a refusal of the
//! input ([`Rejected::refuses_input`]); a crash, a refused syscall or an overrun is
//! this device failing to serve the request, and the next request starts a fresh
//! worker. Only then does the caller make the identical call on its own document,
//! which is deterministic and so does what the worker's did.
//!
//! Workers are long-lived and keep copies of the documents they have seen, keyed by
//! state, each state routed to the worker that last saw it; what a worker keeps is
//! bounded by bytes ([`Limits::cache_bytes`]) and an idle worker exits
//! ([`Limits::idle`]). Every caller shares the workers, one per core the
//! machine's memory can hold at their heap ceiling; a request waits its turn
//! for one, small requests first and callers in turn ([`Caller`]), so one
//! caller's backlog of large documents does not decide another's wait.
//! [`Isolate::load`] takes the change hashes its worker already
//! computed instead of rebuilding them, and leaves the loaded document warm.
//!
//! A worker is the calling executable started again in worker mode, so a program
//! that admits untrusted bytes calls [`dispatch_early`] first thing in `main` (a
//! test crate adds [`test_worker!`]); without it admission fails with
//! [`Rejected::Unavailable`]. A library loaded into a foreign executable ships
//! `tilo-automerge-worker` beside itself and names it with [`serve_workers_from`].
//! A wasm build has no processes: there [`Isolate`] pre-checks and applies in the
//! page, to a copy that replaces the document on success.

pub mod away;
mod deferred;
mod engine;
#[cfg(target_os = "linux")]
mod pool;
pub mod precheck;
#[cfg(target_os = "linux")]
mod sandbox;
#[cfg(target_os = "linux")]
mod turns;
#[cfg(target_os = "linux")]
mod wire;
#[cfg(target_os = "linux")]
mod worker;

use std::time::Duration;

use automerge::AutoCommit;

pub use deferred::Deferred;

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
    /// Worker processes. The default is one per core this process may use,
    /// as many as fit in the machine's memory at [`Limits::memory_bytes`]
    /// each, and at least two.
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
            memory_bytes: MEMORY_BYTES,
            timeout: Duration::from_secs(5),
            timeout_per_mib: Duration::from_secs(2),
            workers: machine_workers(MEMORY_BYTES),
            max_documents: 256,
            cache_bytes: 96 * 1024 * 1024,
            idle: Duration::from_secs(60),
        }
    }
}

const MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// One worker per core this process may use, no more than the machine's
/// memory holds with every worker at its heap ceiling, and at least two.
fn machine_workers(memory_bytes: u64) -> usize {
    let cores = std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get);
    physical_memory()
        .and_then(|memory| usize::try_from(memory.checked_div(memory_bytes)?).ok())
        .map_or(cores, |fit| cores.min(fit))
        .max(2)
}

#[cfg(target_os = "linux")]
fn physical_memory() -> Option<u64> {
    // SAFETY: sysconf only reads system configuration.
    let (pages, page) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    u64::try_from(pages)
        .ok()?
        .checked_mul(u64::try_from(page).ok()?)
}

#[cfg(not(target_os = "linux"))]
const fn physical_memory() -> Option<u64> {
    None
}

/// Whom requests to the workers are made for: workers serve the callers
/// waiting for one in turn, so one caller's backlog never holds another's
/// turn. Each thread is a caller of its own; a thread that works for another
/// one, such as a helper decoding part of that thread's documents, makes its
/// requests as that one's ([`Caller::enter`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Caller(u64);

thread_local! {
    static CALLER: std::cell::Cell<Option<Caller>> = const { std::cell::Cell::new(None) };
}

impl Caller {
    /// Whom this thread's requests are made for.
    #[must_use]
    pub fn current() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        CALLER.with(|caller| {
            caller.get().unwrap_or_else(|| {
                let own = Self(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
                caller.set(Some(own));
                own
            })
        })
    }

    /// Do `work` with this thread's requests made as this caller's.
    pub fn enter<R>(self, work: impl FnOnce() -> R) -> R {
        struct Restore(Option<Caller>);
        impl Drop for Restore {
            fn drop(&mut self) {
                CALLER.with(|caller| caller.set(self.0));
            }
        }
        let _restore = Restore(CALLER.with(|caller| caller.replace(Some(self))));
        work()
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
    /// The bytes are being verified away from this thread ([`away`]): asked
    /// again once that finishes, they load at once.
    #[error("automerge input is being verified")]
    Verifying,
}

impl Rejected {
    /// Whether the input itself was refused. A worker that crashed, overran
    /// its deadline or never started says nothing about the input: it is this
    /// device failing to serve the request, and the next request starts a
    /// fresh worker.
    #[must_use]
    pub fn refuses_input(&self) -> bool {
        matches!(self, Self::TooLarge | Self::Malformed(_) | Self::Invalid(_))
    }
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
        return in_page::load(bytes);
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
/// when it is exactly a run of change chunks — what a member publishes as
/// its own changes since the last take. `None` when it carries anything else,
/// so the caller cannot say which changes are its own and must decide on the
/// whole input instead. Nothing is decoded but each change's own header, and
/// no document is touched: this only names them.
#[must_use]
pub fn change_hashes(incoming: &[u8]) -> Option<Vec<automerge::ChangeHash>> {
    engine::change_hashes(incoming)
}

/// The heads of the document `bytes` saves when it is one whole document, read off its
/// stored form without loading it: naming a whole document written costs a fraction of
/// what loading it does. `None` for anything else, which only loading answers.
#[must_use]
pub fn document_heads(bytes: &[u8]) -> Option<Vec<automerge::ChangeHash>> {
    automerge::stored_document_heads(bytes)
}

/// The changes [`change_hashes`] names, as they are carried: what a caller
/// reading what they write takes instead of building each again from a
/// document that holds them.
#[must_use]
pub fn changes(incoming: &[u8]) -> Option<Vec<automerge::Change>> {
    engine::changes(incoming)
}

/// Every change `bytes` carries, read as [`automerge::Automerge::parse_changes`]
/// reads them: change chunks in order, and the changes of each whole document
/// among them. The framing is checked first ([`precheck`]); a change chunk is
/// read as [`changes`] reads it, and a whole document is loaded as [`load`]
/// loads one, never in this process on Linux. Empty input carries no change.
///
/// # Errors
/// A [`Rejected`] reason: framing past the limits or malformed, a change
/// chunk that does not decode, or a document that does not load.
pub fn parse_changes(bytes: &[u8]) -> Result<Vec<automerge::Change>, Rejected> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    precheck::check(bytes, Isolate::shared().limits())?;
    let mut parsed = Vec::new();
    for chunk in precheck::chunks(bytes) {
        if matches!(chunk.kind, precheck::CHANGE | precheck::COMPRESSED_CHANGE) {
            parsed.push(
                automerge::Change::try_from(chunk.bytes)
                    .map_err(|_| Rejected::Malformed("a change chunk does not decode"))?,
            );
        } else {
            let mut document = load(chunk.bytes)?;
            parsed.extend(document.get_changes(&[]));
        }
    }
    Ok(parsed)
}

/// Outside Linux there are no worker processes, so none of their bounds
/// apply: input that passes [`precheck`] is decoded in this process with no
/// heap ceiling, deadline or sandbox, and a decoder panic or hang is the
/// caller's. Only the copy-then-replace keeps a refused input from changing
/// `doc`. A large document whose changes a host verified away from this
/// thread ([`away`]) loads with the hashes that verification computed.
#[cfg_attr(
    target_os = "linux",
    allow(
        dead_code,
        reason = "Linux applies in worker processes; tests run this"
    )
)]
mod in_page {
    use super::{Applied, AutoCommit, Dependencies, Rejected, away, engine};

    /// Apply to a copy that replaces `doc` only on success.
    ///
    /// A whole document into a blank one is the document a reader on this
    /// thread already loaded from the same bytes, when one did
    /// (`tilo_automerge::shared`): a page reads a record's bytes several
    /// times as it opens it, and loads them once.
    pub(super) fn apply(
        doc: &mut AutoCommit,
        incoming: &[u8],
        dependencies: Dependencies,
    ) -> Result<Applied, Rejected> {
        let whole = engine::is_blank(doc) && engine::single_document(incoming);
        if whole && let Some(held) = tilo_automerge::shared::held(incoming) {
            let actor = doc.get_actor().clone();
            *doc = held.with_actor(actor);
            return Ok(Applied::Changed(!doc.get_heads().is_empty()));
        }
        let mut candidate = doc.clone();
        let applied = match whole.then(|| away::hashes_for(incoming)).transpose()? {
            Some(Some(hashes)) => {
                engine::apply_validated(&mut candidate, incoming, dependencies, &hashes)?
            }
            _ => engine::apply(&mut candidate, incoming, dependencies)?,
        };
        if applied != Applied::MissingDependencies {
            if whole {
                tilo_automerge::shared::hold(incoming, &candidate);
            }
            *doc = candidate;
        }
        Ok(applied)
    }

    /// Load `bytes` as a complete document.
    pub(super) fn load(bytes: &[u8]) -> Result<AutoCommit, Rejected> {
        if let Some(held) = tilo_automerge::shared::held(bytes) {
            return Ok(held);
        }
        let doc = match away::hashes_for(bytes)? {
            Some(hashes) => engine::load_validated(bytes, &hashes),
            None => engine::load(bytes),
        }?;
        if engine::single_document(bytes) {
            tilo_automerge::shared::hold(bytes, &doc);
        }
        Ok(doc)
    }
}

/// The hash of every change of the document `bytes` hold, in the order the
/// document lists them, after [`load`] would have taken them: every change
/// rebuilt and hashed, the stored heads checked against them. What a host's
/// verifier ([`away::Verifier`]) computes away from the caller's thread.
///
/// # Errors
/// A [`Rejected`] reason; [`Rejected::Malformed`] for anything but exactly
/// one document chunk.
pub fn verified_hashes(bytes: &[u8]) -> Result<Vec<automerge::ChangeHash>, Rejected> {
    precheck::check(bytes, Isolate::shared().limits())?;
    engine::verified_hashes(bytes)
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
    let _ = SERVES_FROM_MAIN.set(());
    serve_if_requested()
}

fn serve_if_requested() -> Option<u8> {
    #[cfg(target_os = "linux")]
    if let Some(config) = worker::requested() {
        return Some(u8::try_from(worker::serve(config)).unwrap_or(1));
    }
    None
}

/// Set once this executable's `main` called [`dispatch_early`]: its workers
/// start with the worker argument alone. Otherwise the executable is a test
/// harness serving them through [`test_worker!`], whose workers also carry the
/// arguments that make libtest take that one test and nothing else.
static SERVES_FROM_MAIN: std::sync::OnceLock<()> = std::sync::OnceLock::new();

#[cfg(target_os = "linux")]
/// The arguments a worker of this executable starts with.
pub(crate) fn worker_args() -> &'static [&'static str] {
    if SERVES_FROM_MAIN.get().is_some() || WORKER_PROGRAM.get().is_some() {
        &[worker::ARG]
    } else {
        &[worker::ARG, "--nocapture", "--test-threads=1", "--quiet"]
    }
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
    if let Some(code) = serve_if_requested() {
        #[expect(
            clippy::exit,
            reason = "the worker's test must not return to the test harness"
        )]
        std::process::exit(i32::from(code));
    }
}

#[cfg(test)]
test_worker!();

#[cfg(test)]
mod away_tests;
