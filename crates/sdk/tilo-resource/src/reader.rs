//! Keeping a resource's state current as its document changes, reading only
//! what changed and what views look at.

use std::any::Any;
use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;
use tilo_sort::CompactSort as _;

use automerge::ChangeHash;
use automerge::transaction::Transactable;

use crate::delta::{Delta, Tracker};
use crate::model::{Doc, ModelError};
use crate::resource::Resource;

/// The parts of a large resource that views are looking at, in terms any
/// resource can interpret: positions in its primary order (the paragraphs of
/// a document, the rows of a sheet) and keyed parts (a comment thread, a
/// clip). See [`Resource::load`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Demand {
    /// Positions in the resource's primary order.
    pub ranges: Vec<Range<usize>>,
    /// Keyed parts.
    pub keys: BTreeSet<String>,
}

impl Demand {
    /// Nothing beyond what [`Resource::read`] reads.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// One range of positions.
    #[must_use]
    pub fn range(range: Range<usize>) -> Self {
        Self {
            ranges: vec![range],
            keys: BTreeSet::new(),
        }
    }

    /// Also demand keyed parts.
    #[must_use]
    pub fn with_keys<K: Into<String>>(mut self, keys: impl IntoIterator<Item = K>) -> Self {
        self.keys.extend(keys.into_iter().map(Into::into));
        self
    }

    pub fn covers(&self, index: usize) -> bool {
        self.ranges.iter().any(|range| range.contains(&index))
    }

    /// Every demanded position, ascending and without repeats, clamped to
    /// `len` positions.
    pub fn positions(&self, len: usize) -> impl Iterator<Item = usize> {
        let mut positions: Vec<usize> = self
            .ranges
            .iter()
            .flat_map(|range| range.start.min(len)..range.end.min(len))
            .collect();
        positions.compact_sort();
        positions.dedup();
        positions.into_iter()
    }
}

/// The version `doc` holds, when it has no uncommitted edits.
#[must_use]
pub fn heads_of(doc: &Doc) -> Option<Vec<ChangeHash>> {
    (doc.pending_ops() == 0).then(|| doc.base_heads())
}

/// The current state of one resource over one live document, kept up to date
/// incrementally: each [`sync`](Self::sync) applies only what changed since
/// the last one ([`Resource::refresh`]), and [`demand`](Self::demand) reads
/// the parts views look at ([`Resource::load`]).
///
/// The framework keeps one per open resource; a custom surface running
/// outside the framework (a standalone test harness) can keep its own.
pub struct Reader<R: Resource> {
    /// Everything but the state: the same for every resource.
    core: Following,
    state: Rc<R::State>,
}

/// What a reader keeps besides the state itself.
struct Following {
    tracker: Tracker,
    demand: Demand,
    revision: u64,
    /// How many times a demand read more of the document: what the state
    /// holds of it, not what it says.
    loads: u64,
    /// The last refresh failed part-way; the next sync reads everything.
    stale: bool,
    /// The open transaction last read: its base heads and length.
    uncommitted: Option<(Vec<ChangeHash>, usize)>,
    /// What the last sync that changed the state applied.
    last: Delta,
    /// Why the document last failed to read, until it reads again.
    unreadable: Option<crate::Unreadable>,
}

impl Following {
    /// Following a document from `heads` (`None` when unknown: the first
    /// sync reads everything again). Out of line, so every app's reader
    /// calls the one copy in the runtime.
    #[inline(never)]
    fn at(heads: Option<Vec<ChangeHash>>) -> Self {
        Self::new(heads.map(Tracker::at).unwrap_or_default())
    }

    /// Following `doc` from where it stands, out of line as [`Self::at`].
    #[inline(never)]
    fn started(doc: &mut Doc) -> Self {
        let mut tracker = Tracker::new();
        tracker.start(doc);
        Self::new(tracker)
    }

    fn new(tracker: Tracker) -> Self {
        Self {
            tracker,
            demand: Demand::none(),
            revision: 0,
            loads: 0,
            stale: false,
            uncommitted: None,
            last: Delta::everything(),
            unreadable: None,
        }
    }
}

/// A resource's [`Resource`] functions a reader calls, the state erased.
struct StateOps {
    read: fn(&Doc) -> Result<Box<dyn Any>, ModelError>,
    refresh: fn(&mut dyn Any, &Doc, &Delta) -> Result<(), ModelError>,
    load: fn(&mut dyn Any, &Doc, &Demand) -> Result<(), ModelError>,
    locate: fn(&dyn Any, &automerge::ObjId) -> Option<crate::delta::ObjPath>,
    /// [`Resource::VERSION`].
    version: u32,
}

struct OpsOf<R>(std::marker::PhantomData<R>);

impl<R: Resource> OpsOf<R> {
    const OPS: StateOps = StateOps {
        read: |doc| R::read(doc).map(|state| Box::new(state) as Box<dyn Any>),
        refresh: |state, doc, delta| match state.downcast_mut::<R::State>() {
            Some(state) => R::refresh(state, doc, delta),
            None => Err(ModelError::invalid("a state of another resource")),
        },
        load: |state, doc, demand| match state.downcast_mut::<R::State>() {
            Some(state) => R::load(state, doc, demand),
            None => Err(ModelError::invalid("a state of another resource")),
        },
        locate: |state, obj| R::locate(state.downcast_ref::<R::State>()?, obj),
        version: R::VERSION,
    };
}

/// A reader's state as its resource-independent part reaches it.
trait StateSlot {
    fn get(&self) -> &dyn Any;
    /// The state to change in place, when nobody else holds it.
    fn get_mut(&mut self) -> Option<&mut dyn Any>;
    /// A copy of the state to bring up to date ([`Resource::copy_state`]).
    fn copy(&self) -> Option<Box<dyn Any>>;
    /// Replace the state with `state`, one of the resource's.
    fn set(&mut self, state: Box<dyn Any>);
}

/// A reader's typed state, as a [`StateSlot`].
struct Held<'a, R: Resource>(&'a mut Rc<R::State>);

impl<R: Resource> StateSlot for Held<'_, R> {
    fn get(&self) -> &dyn Any {
        &**self.0
    }

    fn get_mut(&mut self) -> Option<&mut dyn Any> {
        Rc::get_mut(self.0).map(|state| state as &mut dyn Any)
    }

    fn copy(&self) -> Option<Box<dyn Any>> {
        R::copy_state(self.0).map(|state| Box::new(state) as Box<dyn Any>)
    }

    fn set(&mut self, state: Box<dyn Any>) {
        if let Ok(state) = state.downcast::<R::State>() {
            *self.0 = Rc::from(state);
        }
    }
}

impl<R: Resource> Reader<R> {
    /// Read the resource from `doc` and start following its changes.
    pub fn open(doc: &mut Doc) -> Result<Self, ModelError> {
        let mut state = R::read(doc)?;
        R::load(&mut state, doc, &Demand::none())?;
        Ok(Self {
            core: Following::started(doc),
            state: Rc::new(state),
        })
    }

    /// Read the resource from a document that may only be read, and follow
    /// its changes from the next [`sync`](Self::sync) on.
    pub fn read(doc: &Doc) -> Result<Self, ModelError> {
        let mut state = R::read(doc)?;
        R::load(&mut state, doc, &Demand::none())?;
        Ok(Self::with_state(state, heads_of(doc)))
    }

    /// Follow a document from a state already read from it at `heads`
    /// (`None` when unknown: the first sync reads everything again).
    pub fn with_state(state: R::State, heads: Option<Vec<ChangeHash>>) -> Self {
        Self {
            core: Following::at(heads),
            state: Rc::new(state),
        }
    }

    /// The current state. Cheap: shares the value.
    ///
    /// Holding the returned `Rc` across the next [`sync`](Self::sync) keeps
    /// what it shows unchanged: that sync brings a copy up to date instead
    /// ([`Resource::copy_state`]). The copy shares what the change did not
    /// touch: every [`Collection`](crate::Collection) item, the items of each
    /// [`Keyed`](crate::Keyed) or [`Ordered`](crate::Ordered) field (a
    /// touched one is copied once), and the chunks of each
    /// [`Map`](crate::Map), [`Set`](crate::Set) and [`Grid`](crate::Grid).
    /// Plain fields (a `Vec`, a `BTreeMap`) are copied whole: a large one
    /// belongs in one of those, and a view that needs only part of a state
    /// keeps that part rather than the `Rc`.
    pub fn state(&self) -> &Rc<R::State> {
        &self.state
    }

    /// What the last sync that changed the state applied: the parts of the
    /// document it touched, or [`Delta::everything`] when it read the state
    /// whole (an open transaction, a failed refresh, the first read).
    pub fn last_delta(&self) -> &Delta {
        &self.core.last
    }

    /// Why the document does not read as the resource since the last sync,
    /// if it does not: the state is then the last one that did read, and a
    /// view shows this instead of it.
    pub fn unreadable(&self) -> Option<&crate::Unreadable> {
        self.core.unreadable.as_ref()
    }

    /// Increases whenever what the state says changes: the document moved,
    /// or the state is presented again. Reading more of the same document
    /// ([`Self::demand`]) is not a new revision.
    pub fn revision(&self) -> u64 {
        self.core.revision
    }

    /// Increases whenever a demand read parts of the document the state did
    /// not hold yet ([`Self::demand`]).
    pub fn loads(&self) -> u64 {
        self.core.loads
    }

    /// Whether the state is `doc` as it stands: no change arrived since the
    /// last sync.
    pub fn follows(&self, doc: &Doc) -> bool {
        self.core.follows(doc)
    }

    /// Count the unchanged state as a new revision, so everything following
    /// it takes it again (a surface dropping an optimistic preview).
    pub fn represent(&mut self) {
        self.core.revision = self.core.revision.wrapping_add(1);
    }

    /// What views currently demand.
    pub fn current_demand(&self) -> &Demand {
        &self.core.demand
    }

    /// Apply whatever changed in `doc` since the last sync. Returns whether
    /// the state changed.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document no longer holds this
    /// resource; the state is left as it was last read successfully and the
    /// next sync reads it whole.
    #[inline]
    pub fn sync(&mut self, doc: &Doc) -> Result<bool, ModelError> {
        self.core
            .sync(&OpsOf::<R>::OPS, &mut Held::<R>(&mut self.state), doc)
    }

    /// Ask for the parts `demand` names (replacing the previous demand) and
    /// read those not read yet. Returns whether the state changed.
    #[inline(never)]
    pub fn demand(&mut self, doc: &Doc, demand: Demand) -> Result<bool, ModelError>
    where
        R::State: Clone,
    {
        if demand == self.core.demand {
            return Ok(false);
        }
        self.core.demand = demand;
        R::load(Rc::make_mut(&mut self.state), doc, &self.core.demand)?;
        self.core.loads = self.core.loads.wrapping_add(1);
        Ok(true)
    }

    /// Read the state whole from `doc`, whatever the last sync saw move: so a
    /// change this reader could not apply as a delta is reported
    /// ([`Self::unreadable`]) rather than passed over, which a resource kept
    /// small enough to read whole can afford on every sync. Returns whether
    /// the state changed.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document no longer holds this
    /// resource; the state is left as it was last read successfully.
    pub fn reread_whole(&mut self, doc: &Doc) -> Result<bool, ModelError> {
        let before = self.core.revision;
        self.core.stale = true;
        self.core
            .sync(&OpsOf::<R>::OPS, &mut Held::<R>(&mut self.state), doc)?;
        Ok(self.core.revision != before)
    }
}

impl Following {
    fn follows(&self, doc: &Doc) -> bool {
        !self.stale
            && self.uncommitted.is_none()
            && heads_of(doc).is_some_and(|heads| self.tracker.heads() == Some(heads.as_slice()))
    }

    /// [`Reader::sync`].
    fn sync(
        &mut self,
        ops: &StateOps,
        state: &mut dyn StateSlot,
        doc: &Doc,
    ) -> Result<bool, ModelError> {
        // An open transaction belongs to whoever opened it, and closing it
        // here would move their commit boundary. It has no version to diff
        // from, so its state is read whole — once per change of the
        // transaction — and the next sync after it closes diffs as usual.
        if doc.pending_ops() > 0 {
            let open = (doc.base_heads(), doc.pending_ops());
            if self.uncommitted.as_ref() == Some(&open) {
                return Ok(false);
            }
            self.uncommitted = Some(open);
            self.apply(ops, state, doc, &Delta::everything())?;
            return Ok(true);
        }
        let held = state.get();
        let delta = self
            .tracker
            .committed_delta_located(doc, &|obj| (ops.locate)(held, obj));
        let delta = if self.uncommitted.take().is_some() && delta.is_empty() {
            // The transaction was rolled back: read what is left.
            Delta::everything()
        } else {
            delta
        };
        let delta = if self.stale {
            Delta::everything()
        } else {
            delta
        };
        if delta.is_empty() {
            return Ok(false);
        }
        self.apply(ops, state, doc, &delta)?;
        Ok(true)
    }

    fn apply(
        &mut self,
        ops: &StateOps,
        state: &mut dyn StateSlot,
        doc: &Doc,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        let applied = self.apply_delta(ops, state, doc, delta);
        self.unreadable = applied.as_ref().err().map(|error| crate::Unreadable {
            reason: error.to_string(),
            stored: crate::evolution::stored_version(doc),
            expected: ops.version,
        });
        applied
    }

    fn apply_delta(
        &mut self,
        ops: &StateOps,
        state: &mut dyn StateSlot,
        doc: &Doc,
        delta: &Delta,
    ) -> Result<(), ModelError> {
        self.stale = true;
        let mut applied = delta.clone();
        let replaced = match state.get_mut() {
            Some(held) if !delta.is_everything() => {
                if (ops.refresh)(held, doc, delta).is_err() {
                    let mut read = (ops.read)(doc)?;
                    (ops.load)(read.as_mut(), doc, &self.demand)?;
                    applied = Delta::everything();
                    Some(read)
                } else {
                    (ops.load)(held, doc, &self.demand)?;
                    None
                }
            }
            _ => {
                // A view still holds the last state: bring a copy up to date
                // rather than read the whole document again.
                let copied = if delta.is_everything() {
                    None
                } else {
                    crate::profiling::measure("resource.reader_copy", || state.copy())
                };
                let mut fresh = match copied {
                    Some(mut copy) => {
                        if (ops.refresh)(copy.as_mut(), doc, delta).is_err() {
                            copy = (ops.read)(doc)?;
                            applied = Delta::everything();
                        }
                        copy
                    }
                    None => (ops.read)(doc)?,
                };
                (ops.load)(fresh.as_mut(), doc, &self.demand)?;
                Some(fresh)
            }
        };
        self.last = applied;
        if let Some(fresh) = replaced {
            state.set(fresh);
        }
        self.stale = false;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
}
