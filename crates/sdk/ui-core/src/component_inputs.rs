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
enum Key {
    Detached,
    Scope(usize),
    Record(usize, u64),
    Versioned(u64),
    Shared(usize),
}

thread_local! {
    static READING: RefCell<Option<Reading>> = const { RefCell::new(None) };
    /// A pass that must run whatever its witness says: a check that a
    /// skipped pass changed nothing, which reruns every pass it skipped.
    static FORCED: Cell<bool> = const { Cell::new(false) };
    static PROVING: Cell<bool> = const { Cell::new(false) };
}

/// Run `pass` as the pass of the component `owner` names, gathering what it reads outside the
/// component.
pub(crate) fn reading<R>(owner: &WakeHandle, pass: impl FnOnce() -> R) -> (R, Reads) {
    let _feeding = crate::wake::FeedsComponent::begin();
    let pass_reading = PassReading::begin(owner);
    let result = pass();
    (result, pass_reading.end())
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

fn probe(
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

/// A pass read `document`, record `id` of `store`: the pass reads it again once its frontier moved,
/// whoever wrote it.
#[inline(never)]
pub(crate) fn read_record(
    store: &crate::record::RecordStore,
    id: &crate::record::RecordId,
    document: &automerge::AutoCommit,
) {
    let key = Key::Record(store.key(), fold(0, id.as_str().as_bytes()));
    probe(
        || key,
        || {
            let (store, id) = (store.clone(), id.clone());
            (
                Some(frontier(document)),
                Box::new(move || store.read(&id, frontier)),
            )
        },
    );
}

fn frontier(document: &automerge::AutoCommit) -> u64 {
    let (heads, pending) = crate::doc::settle_mark(document);
    heads
        .iter()
        .fold(pending as u64, |folded, head| fold(folded, &head.0))
}

/// `bytes` folded into `folded` (FNV-1a): a key or a frontier as one number,
/// with no hasher instantiated for each type folded.
fn fold(folded: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(folded ^ 0xcbf2_9ce4_8422_2325, |folded, byte| {
            (folded ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// A pass read the settings document `document`, whose version `version` answers: the pass reads it
/// again once that version moved.
#[inline(never)]
pub(crate) fn read_versioned(
    document: &crate::storage::SettingsDocument,
    version: Box<dyn Fn() -> Option<u64>>,
) {
    let named = match document {
        crate::storage::SettingsDocument::Session => 0,
        crate::storage::SettingsDocument::Profile(root) => {
            fold(1, root.as_os_str().as_encoded_bytes())
        }
    };
    probe(|| Key::Versioned(named), || (version(), version));
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

/// Run `check` with every component pass running whatever its witness says: a check that a skipped pass
/// changed nothing, or a test host's frame that is input it delivered past the framework.
pub fn forcing<R>(check: impl FnOnce() -> R) -> R {
    let _forcing = Forcing::begin();
    check()
}

/// Every component pass runs whatever its witness says while this lives ([`forcing`]).
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

/// Whether passes run regardless of their witness ([`forcing`]).
#[inline(never)]
pub(crate) fn forced() -> bool {
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
pub(crate) struct Reads {
    probes: Vec<Probe>,
    untracked: Option<&'static str>,
}

impl Reads {
    /// What `self` and a later pass's `other` read together, each as first read.
    pub(crate) fn join(mut self, other: Self) -> Self {
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
    pub(crate) fn stand(&self) -> Result<(), &'static str> {
        if let Some(what) = self.untracked {
            return Err(what);
        }
        if self.probes.iter().any(|probe| (probe.now)() != probe.read) {
            return Err("ui.component.loud.reads");
        }
        Ok(())
    }
}

/// Everything a component's last pass read, as it stood once the pass ended.
pub(crate) struct Witness {
    pub(crate) revision: u64,
    /// Its record's committed heads and what is pending on top of them.
    pub(crate) document: (Vec<automerge::ChangeHash>, usize),
    pub(crate) standing: Standing,
    pub(crate) reads: Reads,
}

/// What a component's pass reads of its host besides its record: what its permissions mean now and its
/// record's save status (what its root was last told, in a witness; what the store says, now), as a
/// frame's quiet check reads them, and the locale.
#[derive(PartialEq)]
pub(crate) struct Standing {
    pub(crate) owner: crate::bound_dispatch::OwnerStanding,
    pub(crate) locale: u64,
}

impl Witness {
    /// Why a pass reading `now` would read something else than the pass this witness was taken after,
    /// or `None` when it would read the same.
    pub(crate) fn moved(&self, now: &Now) -> Option<&'static str> {
        if self.document.1 != 0 {
            return Some("ui.component.loud.uncommitted");
        }
        if now.revision != Some(self.revision) {
            return Some("ui.component.loud.input");
        }
        if now.document.as_ref() != Some(&self.document) {
            return Some("ui.component.loud.document");
        }
        if now.standing != self.standing {
            return Some("ui.component.loud.standing");
        }
        self.reads.stand().err()
    }
}

/// What a component's witness is compared with: the same readings, now.
pub(crate) struct Now {
    pub(crate) revision: Option<u64>,
    pub(crate) document: Option<(Vec<automerge::ChangeHash>, usize)>,
    pub(crate) standing: Standing,
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
}
