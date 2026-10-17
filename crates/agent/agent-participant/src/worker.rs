//! App workers: where an app's code runs when the host acts on a room's
//! records, out of the host's trust domain.
//!
//! The host holds its device key, every agent's device key and every room
//! it serves. An app's code — its resource kind, its seed, its component —
//! runs over whatever a room's members put in a record, so it does not run
//! there. Each record the host's room actions or an agent holds lives in
//! its own worker process ([`ConfinedReplica`]); the registered reads of a
//! record's component ([`ConfinedRead`]) and the views of an existing
//! resource ([`ConfinedViews`]) each run in one too.
//!
//! A worker is the host's own executable started again in worker mode. It
//! prepares what it cannot load later (the app catalogue; a reader's
//! fonts), then confines itself before it reads a request
//! (`worker/confine.rs`): no path can be opened or changed, no socket
//! made, no program started and no other process signalled, so the host's
//! state directory and keys are out of its reach. A worker that cannot
//! confine itself serves nothing, and the operation is refused: there is no unconfined
//! fallback.
//!
//! The protocol (`worker/proto.rs`) carries app ids, record ids, the
//! participant's public name, Automerge bytes and the policy a caller holds
//! — never a key. What a worker returns is untrusted: Automerge bytes are
//! admitted through `tilo-automerge-isolate` before the host signs them
//! ([`ConfinedReplica::take_mutation`]).
//!
//! # Hosting workers
//!
//! Every program that runs room actions or agents calls [`dispatch_early`]
//! first thing in `main`, before it starts threads or a runtime. A test
//! crate adds [`app_worker!`](crate::app_worker) instead. Without either,
//! opening a record fails, naming the missing line.

mod confine;
mod process;
pub(crate) mod proto;
mod read;
mod replica;
mod serve;

pub use proto::ProbeReport;
pub(crate) use read::{ConfinedRead, ConfinedViews};
pub use replica::ConfinedReplica;

/// The first argument of a worker. It is also the name of the test
/// [`app_worker!`](crate::app_worker) defines, so libtest runs exactly that
/// test.
pub(crate) const ARG: &str = "tilo_agent_app_worker";
/// Set in a worker's environment to its mode.
pub(crate) const ENV: &str = "TILO_AGENT_APP_WORKER";

/// Serve as an app worker when this process was started as one, returning
/// its exit code; `None` on an ordinary launch.
///
/// Call it first thing in `main`, before starting threads or a runtime:
///
/// ```no_run
/// fn main() -> std::process::ExitCode {
///     if let Some(code) = tilo_agent_participant::worker::dispatch_early() {
///         return std::process::ExitCode::from(code);
///     }
///     // ...
///     std::process::ExitCode::SUCCESS
/// }
/// ```
#[must_use]
pub fn dispatch_early() -> Option<u8> {
    // The argument and the environment together, so a stray variable alone
    // never diverts a program.
    if !std::env::args_os().any(|arg| arg == ARG) {
        return None;
    }
    let mode = proto::Mode::parse(&std::env::var(ENV).ok()?)?;
    Some(u8::try_from(serve::serve(mode)).unwrap_or(1))
}

/// Serve app workers from a test binary, whose `main` belongs to libtest:
/// defines the test that libtest runs when the binary is started as a
/// worker, which then does what [`dispatch_early`] does. Invoke once per
/// test crate (for unit tests, in the library under `#[cfg(test)]`; for
/// each integration test file, at its top level).
#[macro_export]
macro_rules! app_worker {
    () => {
        #[test]
        fn tilo_agent_app_worker() {
            $crate::worker::serve_test_worker();
        }
    };
}

/// What a program's main thread gets on Linux. A worker started by a program
/// serves on its main thread; one started by a test binary would otherwise
/// serve on a libtest thread with Rust's 2 MiB default, which an app's
/// worker can outgrow.
const WORKER_STACK: usize = 8 * 1024 * 1024;

#[doc(hidden)]
pub fn serve_test_worker() {
    // A thread that cannot start or panics fails the worker, as a failed
    // main would.
    let served = std::thread::Builder::new()
        .name(ARG.into())
        .stack_size(WORKER_STACK)
        .spawn(dispatch_early)
        .map_or(Some(1), |thread| thread.join().unwrap_or(Some(101)));
    if let Some(code) = served {
        #[expect(
            clippy::exit,
            reason = "the worker's test must not return to the test harness"
        )]
        std::process::exit(i32::from(code));
    }
}

/// Start a worker and report what its confinement still lets it do at
/// `path`. Every field of the report is `false` for a confined worker.
///
/// # Errors
///
/// The worker did not start or could not confine itself.
pub fn probe_confinement(path: &std::path::Path) -> Result<ProbeReport, String> {
    let mut process = process::Process::start(proto::Mode::Replica)?;
    match process.call(&proto::Request::Probe {
        path: path.to_string_lossy().into_owned(),
    })? {
        proto::Reply::Probe(report) => Ok(report),
        _ => Err("the app worker answered out of turn".into()),
    }
}

#[cfg(test)]
crate::app_worker!();
