//! Retained-store primitives a Tilo workspace host shares with its shell
//! replay adapter.
//!
//! The relay core (`tilo-relay`) owns a workspace's on-disk store. Native
//! shell replay selection (`tilo-relay-shell`) reads and writes the same
//! retained tree under the desktop's retention proofs. Both build on what is
//! here:
//!
//! - [`bounded`]: disk-spilled metadata ordering and bounded work quanta;
//! - [`durable`]: counted file and directory barriers, generation selectors
//!   (`CURRENT`) and bounded generation collection;
//! - [`frames`]: complete retained relay frames and their co-edit payloads;
//! - [`tail_cache`]: the per-root continuation a clean shell selection leaves
//!   for the next route tail;
//! - [`shell`]: the seam the store drives shell replay through. A store holds
//!   an opaque [`shell::ShellBinding`] and never names the proof types behind
//!   it.
//!
//! Every barrier the store and its adapter issue is counted in [`fsyncs`].

pub mod bounded;
pub mod durable;
pub mod frames;
pub mod shell;
pub mod tail_cache;
#[cfg(any(test, feature = "test-support"))]
pub mod test_barrier;

use std::fs;
use std::io;
use std::path::Path;

use thiserror::Error;

/// Errors from store operations.
#[derive(Debug, Error)]
pub enum StoreError {
    /// I/O failure.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// JSON encode/decode failure for a metadata file.
    #[error("json: {0}")]
    Json(String),
}

/// fsyncs the relay's workspace stores issued since this process started.
static FSYNCS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// [`FSYNCS`] of this thread only: tests run in parallel, and a store
    /// driven synchronously issues its barriers on the calling thread.
    static THREAD_FSYNCS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `tilo_files::native::atomic::write` syncs the staged file, and after the
/// rename the directory holding it.
const ATOMIC_WRITE_FSYNCS: u64 = 2;

fn count_fsyncs(count: u64) {
    FSYNCS.fetch_add(count, std::sync::atomic::Ordering::Relaxed);
    #[cfg(any(test, feature = "test-support"))]
    THREAD_FSYNCS.with(|fsyncs| fsyncs.set(fsyncs.get().saturating_add(count)));
}

/// fsyncs (file and directory barriers) the relay's workspace stores have
/// issued in this process: the cost the hosting bench divides by edits.
#[must_use]
pub fn fsyncs() -> u64 {
    FSYNCS.load(std::sync::atomic::Ordering::Relaxed)
}

/// [`fsyncs`] issued on the calling thread.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn thread_fsyncs() -> u64 {
    THREAD_FSYNCS.with(std::cell::Cell::get)
}

/// The barriers this thread issued on behalf of another, which
/// [`credit_fsyncs`] counts on that one: a store's work stays countable on
/// its calling thread when it spreads independent barriers over helpers.
#[must_use]
pub fn helper_fsyncs() -> u64 {
    #[cfg(any(test, feature = "test-support"))]
    return thread_fsyncs();
    #[cfg(not(any(test, feature = "test-support")))]
    0
}

/// Count `count` barriers a helper thread issued for the calling one.
pub fn credit_fsyncs(count: u64) {
    #[cfg(any(test, feature = "test-support"))]
    THREAD_FSYNCS.with(|fsyncs| fsyncs.set(fsyncs.get().saturating_add(count)));
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = count;
}

/// What must hold before a barrier on this thread orders anything after it.
type BarrierHook = Box<dyn FnMut() -> io::Result<()>>;

thread_local! {
    /// Set by [`with_barrier_hook`] while a workspace owner's quantum runs.
    static BEFORE_BARRIER: std::cell::RefCell<Option<BarrierHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `operation` with `hook` called before every barrier it issues on
/// this thread ([`fsync`], [`atomic_write`], [`before_barrier`]).
///
/// A workspace owner that writes records whose barrier it defers (a group
/// commit) makes them durable in its hook: whatever a later barrier makes
/// durable is then ordered after them, so a crash can never keep a write
/// that depends on a deferred record and lose the record.
pub fn with_barrier_hook<R>(
    hook: impl FnMut() -> io::Result<()> + 'static,
    operation: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<BarrierHook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            BEFORE_BARRIER.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    let previous = BEFORE_BARRIER.with(|slot| slot.borrow_mut().replace(Box::new(hook)));
    let _restore = Restore(previous);
    operation()
}

/// Call this thread's barrier hook ([`with_barrier_hook`]), if one is set.
/// Code that spreads barriers over helper threads calls it first on its own.
///
/// # Errors
///
/// Returns the hook's error.
pub fn before_barrier() -> io::Result<()> {
    // Taken out while it runs: the barriers it issues itself do not recurse.
    let Some(mut hook) = BEFORE_BARRIER.with(|slot| slot.borrow_mut().take()) else {
        return Ok(());
    };
    let result = hook();
    BEFORE_BARRIER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(hook);
        }
    });
    result
}

/// `file.sync_all()`, counted in [`fsyncs`].
///
/// # Errors
///
/// Returns the error `sync_all` reports.
pub fn fsync(file: &fs::File) -> io::Result<()> {
    before_barrier()?;
    count_fsyncs(1);
    file.sync_all()
}

/// `tilo_files::native::atomic::write`, counted in [`fsyncs`].
///
/// # Errors
///
/// Returns the error the atomic write reports.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    before_barrier()?;
    count_fsyncs(ATOMIC_WRITE_FSYNCS);
    tilo_files::native::atomic::write(path, bytes)
}
