//! A thread's stack, captured by its own signal handler without allocating or
//! locking.
//!
//! The dumping thread gives each thread it is about to signal a slot labelled
//! with that thread's id and the dump's generation. The handler finds its own
//! slot by that label, records the return addresses of its stack into it and
//! marks it answered last. Nothing another handler does can take a slot away
//! from it, so every answer is kept, and a slot not marked answered is a
//! thread that did not answer. Symbols are resolved afterwards, by the dumping
//! thread.
//!
//! Slots are never freed: a table outgrown by the process's thread count is
//! replaced by a larger one and the old one leaked, so a handler still running
//! from a dump that gave up waiting for it writes into memory that is still
//! its slot.

use std::ffi::c_void;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

/// Return addresses kept per thread: deeper stacks keep their innermost.
const FRAMES: usize = 128;

struct Slot {
    tid: AtomicI32,
    /// The generation of the dump that asks this slot's thread.
    asked: AtomicU64,
    /// The generation of the handler writing the slot.
    claimed: AtomicU64,
    /// The generation whose frames the slot holds, written once they are.
    answered: AtomicU64,
    /// The first frame of the interrupted code: those before it are the
    /// handler's own and the kernel's signal trampoline.
    start: AtomicUsize,
    len: AtomicUsize,
    frames: [AtomicUsize; FRAMES],
}

impl Slot {
    fn new() -> Self {
        Self {
            tid: AtomicI32::new(0),
            asked: AtomicU64::new(0),
            claimed: AtomicU64::new(0),
            answered: AtomicU64::new(0),
            start: AtomicUsize::new(0),
            len: AtomicUsize::new(0),
            frames: std::array::from_fn(|_| AtomicUsize::new(0)),
        }
    }

    /// No handler is writing it: every claim has been answered.
    fn idle(&self) -> bool {
        self.claimed.load(Ordering::Acquire) == self.answered.load(Ordering::Acquire)
    }
}

/// The slots, leaked so a handler may hold them for the life of the process.
static TABLE: AtomicPtr<Vec<Slot>> = AtomicPtr::new(std::ptr::null_mut());
/// The dump asking now; 0 when none is.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static LAST_GENERATION: AtomicU64 = AtomicU64::new(0);

fn table() -> Option<&'static [Slot]> {
    let table = TABLE.load(Ordering::Acquire);
    // SAFETY: a published table is leaked and never freed or mutated except
    // through its atomics.
    (!table.is_null()).then(|| unsafe { (*table).as_slice() })
}

/// The capture signal's handler: records this thread's stack into the slot
/// the asking dump labelled with its id. Only atomics, `gettid` and the
/// unwinder, which reads the stack and the unwind tables it found when
/// [`warm_up`] ran.
pub(super) extern "C" fn answer(_signal: libc::c_int) {
    // SAFETY: `__errno_location` returns this thread's errno, which the
    // interrupted code may be about to read.
    let errno = unsafe { *libc::__errno_location() };
    record_own_stack();
    // SAFETY: as above.
    unsafe { *libc::__errno_location() = errno };
}

fn record_own_stack() {
    let generation = GENERATION.load(Ordering::Acquire);
    if generation == 0 {
        return;
    }
    let Some(slots) = table() else {
        return;
    };
    let tid = super::gettid();
    let Some(slot) = slots.iter().find(|slot| {
        slot.tid.load(Ordering::Acquire) == tid && slot.asked.load(Ordering::Acquire) == generation
    }) else {
        return;
    };
    if slot.claimed.swap(generation, Ordering::AcqRel) == generation {
        // Already answered for this dump.
        return;
    }
    let handler = answer as extern "C" fn(libc::c_int) as *mut c_void;
    let mut len = 0;
    // Until this handler's own frame is passed, its frames are kept only in
    // case it is not found; the frame after it is the signal trampoline.
    let mut start = 0;
    // SAFETY: tracing walks this thread's own stack; no other thread traces
    // through this slot, which this handler claimed.
    unsafe {
        backtrace::trace_unsynchronized(|frame| {
            let ip = frame.ip().addr();
            if ip == 0 {
                return false;
            }
            let Some(cell) = slot.frames.get(len) else {
                return false;
            };
            cell.store(ip, Ordering::Relaxed);
            len = len.saturating_add(1);
            if start == 0 && frame.symbol_address() == handler {
                start = len.saturating_add(1);
            }
            true
        });
    }
    slot.start.store(start.min(len), Ordering::Relaxed);
    slot.len.store(len, Ordering::Relaxed);
    slot.answered.store(generation, Ordering::Release);
}

/// Load what the unwinder needs before any thread is interrupted, so the
/// first capture finds it ready instead of setting it up inside a handler.
pub(super) fn warm_up() {
    backtrace::trace(|_| false);
}

/// One dump's question to a set of threads.
pub(super) struct Ask {
    generation: u64,
    slots: &'static [Slot],
    /// Each asked thread's slot.
    asked: Vec<(i32, usize)>,
}

impl Ask {
    /// Label a slot for each of `tids`. The caller holds the one-dump lock
    /// and signals each of them after this returns.
    pub(super) fn new(tids: &[i32]) -> Self {
        let generation = LAST_GENERATION
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            .max(1);
        let slots = slots_for(tids.len());
        let mut free = slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.idle())
            .map(|(index, _)| index);
        let mut asked = Vec::with_capacity(tids.len());
        for &tid in tids {
            let Some(index) = free.next() else {
                break;
            };
            let Some(slot) = slots.get(index) else {
                break;
            };
            slot.tid.store(tid, Ordering::Release);
            slot.asked.store(generation, Ordering::Release);
            asked.push((tid, index));
        }
        GENERATION.store(generation, Ordering::Release);
        Self {
            generation,
            slots,
            asked,
        }
    }

    /// Whether `tid` has a slot to answer into.
    pub(super) fn has_room_for(&self, tid: i32) -> bool {
        self.asked.iter().any(|(asked, _)| *asked == tid)
    }

    fn slot(&self, tid: i32) -> Option<&'static Slot> {
        let index = self
            .asked
            .iter()
            .find(|(asked, _)| *asked == tid)
            .map(|(_, index)| *index)?;
        self.slots.get(index)
    }

    /// Whether `tid` has answered.
    pub(super) fn answered(&self, tid: i32) -> bool {
        self.slot(tid)
            .is_some_and(|slot| slot.answered.load(Ordering::Acquire) == self.generation)
    }

    /// Wait until every one of `tids` has answered or `deadline` passes.
    pub(super) fn wait(&self, tids: &[i32], deadline: Instant) {
        while !tids.iter().all(|&tid| self.answered(tid)) && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// `tid`'s stack where the signal interrupted it, innermost frame first,
    /// symbolized; `None` when it has not answered.
    pub(super) fn frames(&self, tid: i32) -> Option<String> {
        let slot = self
            .slot(tid)
            .filter(|slot| slot.answered.load(Ordering::Acquire) == self.generation)?;
        let len = slot.len.load(Ordering::Relaxed).min(FRAMES);
        let start = slot.start.load(Ordering::Relaxed).min(len);
        let ips: Vec<usize> = slot
            .frames
            .iter()
            .take(len)
            .skip(start)
            .map(|cell| cell.load(Ordering::Relaxed))
            .collect();
        Some(symbolize(&ips))
    }
}

impl Drop for Ask {
    fn drop(&mut self) {
        // A handler arriving after the dump finds nobody asking.
        let _ =
            GENERATION.compare_exchange(self.generation, 0, Ordering::AcqRel, Ordering::Acquire);
    }
}

/// A table with room for `threads` idle slots, grown (and the old one
/// leaked) when the process has more threads than it holds.
fn slots_for(threads: usize) -> &'static [Slot] {
    let current = table();
    let idle = current.map_or(0, |slots| slots.iter().filter(|slot| slot.idle()).count());
    if let Some(slots) = current
        && idle >= threads
    {
        return slots;
    }
    let len = threads
        .saturating_mul(2)
        .max(current.map_or(0, <[Slot]>::len).saturating_mul(2))
        .max(64);
    let grown = Box::into_raw(Box::new((0..len).map(|_| Slot::new()).collect::<Vec<_>>()));
    TABLE.store(grown, Ordering::Release);
    // SAFETY: just leaked, and never freed.
    unsafe { (*grown).as_slice() }
}

/// `  N: symbol` lines, each followed by `             at file:line` where
/// the debug information says, as `std::backtrace` prints them.
fn symbolize(ips: &[usize]) -> String {
    let mut out = String::new();
    let mut index = 0usize;
    for &ip in ips {
        let mut resolved = false;
        backtrace::resolve(std::ptr::without_provenance_mut::<c_void>(ip), |symbol| {
            resolved = true;
            let name = symbol
                .name()
                .map_or_else(|| "<unknown>".to_string(), |name| format!("{name:#}"));
            let _ = writeln!(out, "{index:>4}: {name}");
            if let (Some(file), Some(line)) = (symbol.filename(), symbol.lineno()) {
                let _ = writeln!(out, "             at {}:{line}", file.display());
            }
            index = index.saturating_add(1);
        });
        if !resolved {
            let _ = writeln!(out, "{index:>4}: <unknown> {ip:#x}");
            index = index.saturating_add(1);
        }
    }
    out
}
