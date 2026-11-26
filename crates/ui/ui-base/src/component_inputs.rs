//! What a component's sync reads, kept visible to the framework so that a pass can prove nothing it
//! reads moved and skip it. A window's root syncs from its record, and from state outside it: typed
//! local state its views bind, the answers of work it started off the frame, its permissions and its
//! record's durability.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::wake::WakeHandle;

static DETACHED: AtomicU64 = AtomicU64::new(0);

/// Typed local state `owner` is told of was written: its owner's input revision moves, or, for state of
/// no scope, the detached revision.
#[inline(never)]
pub(crate) fn note_written(owner: &WakeHandle) {
    if owner.is_detached() {
        DETACHED.fetch_add(1, Ordering::AcqRel);
    } else {
        owner.note_change();
        written_by(owner);
    }
}

struct Reading {
    /// Which pass this is ([`running_pass`]), counted on this thread.
    pass: u64,
    owner: WakeHandle,
    probes: Vec<Probe>,
    untracked: Option<&'static str>,
    read_own: bool,
}

struct Probe {
    key: Key,
    read: Option<u64>,
    now: Box<dyn Fn() -> Option<u64>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Detached,
    Scope(usize),
    #[cfg(feature = "model")]
    Record(usize, u64),
    Versioned(u64),
    Shared(usize),
}

thread_local! {
    static READING: RefCell<Option<Reading>> = const { RefCell::new(None) };
    /// A pass that must run whatever its observation says: a check that a
    /// skipped pass changed nothing, which reruns every pass it skipped.
    static FORCED: Cell<bool> = const { Cell::new(false) };
    static PROVING: Cell<bool> = const { Cell::new(false) };
    /// Counts this thread's passes: the number the next one takes.
    static PASSES: Cell<u64> = const { Cell::new(0) };
}

/// Run `pass` as the pass of the component `owner` names, gathering what it reads outside the
/// component.
pub fn reading<R>(owner: &WakeHandle, pass: impl FnOnce() -> R) -> (R, Reads) {
    let _feeding = crate::wake::FeedsComponent::begin();
    let pass_reading = PassReading::begin(owner);
    let result = pass();
    (result, pass_reading.end())
}

/// The number of the pass running, or `None` outside a pass: what a reader of several resources
/// registers the reading it follows once a pass, however often the pass looks at them. A pass that
/// another one nests (a component's published resources, gathered while its window syncs) has
/// its own number, and once it ends the pass around it is the one running again: what the inner pass
/// read is followed by the inner one's component, not by the window's.
#[must_use]
pub fn running_pass() -> Option<u64> {
    READING.with(|reading| {
        reading
            .try_borrow()
            .ok()
            .and_then(|reading| reading.as_ref().map(|reading| reading.pass))
    })
}

/// The reading of one pass, from [`PassReading::begin`] to
/// [`PassReading::end`]: the enclosing pass's reading is restored however the
/// pass ends. Out of line, so every caller of [`reading`] shares one copy.
struct PassReading {
    outer: Option<Reading>,
    crossed: Option<u64>,
    locale: u64,
}

impl PassReading {
    #[inline(never)]
    fn begin(owner: &WakeHandle) -> Self {
        let outer = READING.with(|reading| {
            reading.borrow_mut().replace(Reading {
                pass: PASSES.with(|passes| passes.replace(passes.get().wrapping_add(1))),
                owner: owner.clone(),
                probes: Vec::new(),
                untracked: None,
                read_own: false,
            })
        });
        Self {
            outer,
            crossed: owner.crossed(),
            locale: crate::i18n::I18nManager::revision(),
        }
    }

    #[inline(never)]
    fn end(self) -> Reads {
        let read = READING.with(|reading| reading.borrow_mut().take());
        let crossed = read
            .as_ref()
            .is_some_and(|read| read.owner.crossed() != self.crossed);
        let language = crate::i18n::I18nManager::revision() != self.locale;
        drop(self);
        match read {
            Some(read) => Reads {
                probes: read.probes,
                untracked: read
                    .untracked
                    .or(crossed.then_some("ui.component.loud.woken_during_pass"))
                    .or(language.then_some("ui.component.loud.language_during_pass")),
            },
            None => Reads {
                probes: Vec::new(),
                untracked: Some("component_inputs.lost"),
            },
        }
    }
}

impl Drop for PassReading {
    fn drop(&mut self) {
        let outer = self.outer.take();
        READING.with(|reading| *reading.borrow_mut() = outer);
    }
}

pub fn probe(
    make_key: impl FnOnce() -> Key,
    make: impl FnOnce() -> (Option<u64>, Box<dyn Fn() -> Option<u64>>),
) {
    READING.with(|reading| {
        let Ok(mut reading) = reading.try_borrow_mut() else {
            return;
        };
        let Some(reading) = reading.as_mut() else {
            return;
        };
        let key = make_key();
        if reading.probes.iter().any(|probe| probe.key == key) {
            return;
        }
        let (read, now) = make();
        reading.probes.push(Probe { key, read, now });
    });
}

/// A pass read typed local state whose writes `owner` is told of.
#[inline(never)]
pub(crate) fn read_owned_by(owner: &WakeHandle) {
    let Some(scope) = owner.scope_key() else {
        probe(
            || Key::Detached,
            || {
                let now = || Some(DETACHED.load(Ordering::Acquire));
                (now(), Box::new(now))
            },
        );
        return;
    };
    let own = READING.with(|reading| {
        reading.try_borrow_mut().ok().and_then(|mut reading| {
            let reading = reading.as_mut()?;
            let own = owner.same_open_scope(&reading.owner);
            reading.read_own |= own;
            Some(own)
        })
    });
    if own == Some(false) {
        probe(
            || Key::Scope(scope),
            || {
                let owner = owner.clone();
                (owner.revision(), Box::new(move || owner.revision()))
            },
        );
    }
}

/// `bytes` folded into `folded` (FNV-1a): a key or a frontier as one number,
/// with no hasher instantiated for each type folded.
pub fn fold(folded: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(folded ^ 0xcbf2_9ce4_8422_2325, |folded, byte| {
            (folded ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// Local state `owner` is told of was written.
#[inline(never)]
pub(crate) fn written_by(owner: &WakeHandle) {
    READING.with(|reading| {
        if let Ok(mut reading) = reading.try_borrow_mut()
            && let Some(reading) = reading.as_mut()
            && reading.read_own
            && owner.same_open_scope(&reading.owner)
        {
            reading
                .untracked
                .get_or_insert("ui.component.loud.wrote_what_it_read");
        }
    });
}

/// State several views share that belongs to no component scope and no record: the live resource a
/// surface follows, what a workspace published.
#[derive(Clone)]
pub struct Shared {
    key: usize,
    generation: std::rc::Rc<std::cell::Cell<u64>>,
}

impl Shared {
    /// Shared state that has moved `moves` times so far.
    #[must_use]
    pub fn new(moves: u64) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
        Self {
            key: NEXT.fetch_add(1, Ordering::Relaxed),
            generation: std::rc::Rc::new(std::cell::Cell::new(moves)),
        }
    }

    /// The reading of the state now, which the pass running follows.
    pub fn read(&self) -> u64 {
        let generation = std::rc::Rc::clone(&self.generation);
        probe(
            || Key::Shared(self.key),
            || {
                let now = move || Some(generation.get());
                let first = now();
                (first, Box::new(now))
            },
        );
        self.generation.get()
    }

    /// The state moved: every pass that read it runs again.
    pub fn moved(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }
}

/// The pass running read something the framework cannot follow (`what`
/// names it, for a profile): its component syncs on the next frame whatever
/// else is. Outside a component's pass it does nothing.
#[inline(never)]
pub fn untracked(what: &'static str) {
    READING.with(|reading| {
        if let Ok(mut reading) = reading.try_borrow_mut()
            && let Some(reading) = reading.as_mut()
        {
            reading.untracked.get_or_insert(what);
        }
    });
}

/// Run `check` with every component pass running whatever its observation says: a check that a skipped pass
/// changed nothing, or a test host's frame that is input it delivered past the framework.
pub fn forcing<R>(check: impl FnOnce() -> R) -> R {
    let _forcing = Forcing::begin();
    check()
}

/// Every component pass runs whatever its observation says while this lives ([`forcing`]).
pub struct Forcing(bool);

impl Forcing {
    #[inline(never)]
    #[must_use]
    pub fn begin() -> Self {
        Self(FORCED.with(|forced| forced.replace(true)))
    }
}

impl Drop for Forcing {
    fn drop(&mut self) {
        FORCED.with(|forced| forced.set(self.0));
    }
}

/// Whether passes run regardless of their observation ([`forcing`]).
#[inline(never)]
pub fn forced() -> bool {
    FORCED.with(Cell::get)
}

/// Run `check` as a proof that a pass at rest changes nothing: it syncs without the work a frame takes
/// in beside the tree's sync — what an app hook moves into the state its views are keyed by when
/// finished work arrives.
pub fn proving<R>(check: impl FnOnce() -> R) -> R {
    struct Proving(bool);
    impl Drop for Proving {
        fn drop(&mut self) {
            PROVING.with(|proving| proving.set(self.0));
        }
    }
    let _proving = Proving(PROVING.with(|proving| proving.replace(true)));
    check()
}

/// Whether the pass running is a proof ([`proving`]).
#[inline(never)]
#[must_use]
pub fn is_proving() -> bool {
    PROVING.with(Cell::get)
}

/// What a pass read outside its component ([`reading`]).
pub struct Reads {
    probes: Vec<Probe>,
    untracked: Option<&'static str>,
}

impl Reads {
    /// What `self` and a later pass's `other` read together, each as first read.
    pub fn join(mut self, other: Self) -> Self {
        self.untracked = self.untracked.or(other.untracked);
        self.take(other.probes, false);
        self
    }

    /// What `self`, a later pass, read, and what the earlier passes read that it did not read again: a
    /// view may still show what an earlier pass read, while its checks skip reading it again.
    pub(crate) fn carry(mut self, earlier: Self) -> Self {
        self.take(earlier.probes, true);
        self
    }

    fn take(&mut self, probes: Vec<Probe>, read_now: bool) {
        for mut probe in probes {
            if self.probes.iter().any(|seen| seen.key == probe.key) {
                continue;
            }
            if read_now {
                probe.read = (probe.now)();
            }
            self.probes.push(probe);
        }
    }

    /// Whether every outside read still is where it was read.
    pub fn stand(&self) -> Result<(), &'static str> {
        if let Some(what) = self.untracked {
            return Err(what);
        }
        if self.probes.iter().any(|probe| (probe.now)() != probe.read) {
            return Err("ui.component.loud.reads");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_follows_the_shared_state_it_read() {
        let scope = crate::wake::WakeScope::new();
        let handle = scope.handle();
        let (live, other) = (Shared::new(0), Shared::new(0));
        let ((), reads) = reading(&handle, || {
            live.read();
        });
        assert!(reads.stand().is_ok(), "nothing moved");
        other.moved();
        assert!(reads.stand().is_ok(), "the state it did not read moved");
        live.moved();
        assert_eq!(reads.stand(), Err("ui.component.loud.reads"));
    }

    /// A reader of several resources reads them once a pass ([`running_pass`]), as a chain reads
    /// the parts that Chat's unread counts are summed from. A pass nested in the window's
    /// pass and read them first (the window's published resources, gathered while it syncs) does
    /// not stand in for the window's own: the window still follows them, and wakes when one moves.
    #[test]
    fn a_window_follows_what_a_pass_inside_it_read_first() {
        let scope = crate::wake::WakeScope::new();
        let (window, published) = (scope.handle(), scope.handle());
        let part = Shared::new(0);
        let read_at = Cell::new(None);
        let read_once_a_pass = || {
            let pass = running_pass();
            if pass.is_some() && read_at.get() != pass {
                part.read();
                read_at.set(pass);
            }
        };
        let ((), reads) = reading(&window, || {
            let ((), inner) = reading(&published, read_once_a_pass);
            assert_eq!(inner.stand(), Ok(()));
            read_once_a_pass();
        });
        part.moved();
        assert_eq!(
            reads.stand(),
            Err("ui.component.loud.reads"),
            "the window read the part after the pass inside it did: it wakes when the part moves"
        );
    }
}
