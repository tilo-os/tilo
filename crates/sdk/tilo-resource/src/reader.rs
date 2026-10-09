//! Keeping a resource's state current as its document changes, reading only
//! what changed and what views look at.

use std::collections::BTreeSet;
use std::ops::Range;
use std::rc::Rc;

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

    /// Whether position `index` is demanded.
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
        positions.sort_unstable();
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
    tracker: Tracker,
    state: Rc<R::State>,
    demand: Demand,
    revision: u64,
    /// The last refresh failed part-way; the next sync reads everything.
    stale: bool,
    /// The open transaction last read: its base heads and length.
    uncommitted: Option<(Vec<ChangeHash>, usize)>,
    /// What the last sync that changed the state applied.
    last: Delta,
    /// Why the document last failed to read, until it reads again.
    unreadable: Option<crate::Unreadable>,
}

impl<R: Resource> Reader<R> {
    /// Read the resource from `doc` and start following its changes.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    pub fn open(doc: &mut Doc) -> Result<Self, ModelError> {
        let mut state = R::read(doc)?;
        let demand = Demand::none();
        R::load(&mut state, doc, &demand)?;
        let mut tracker = Tracker::new();
        tracker.start(doc);
        Ok(Self {
            tracker,
            state: Rc::new(state),
            demand,
            revision: 0,
            stale: false,
            uncommitted: None,
            last: Delta::everything(),
            unreadable: None,
        })
    }

    /// Read the resource from a document that may only be read, and follow
    /// its changes from the next [`sync`](Self::sync) on.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    pub fn read(doc: &Doc) -> Result<Self, ModelError> {
        let mut state = R::read(doc)?;
        let demand = Demand::none();
        R::load(&mut state, doc, &demand)?;
        Ok(Self::with_state(state, heads_of(doc)))
    }

    /// Follow a document from a state already read from it at `heads`
    /// (`None` when unknown: the first sync reads everything again).
    pub fn with_state(state: R::State, heads: Option<Vec<ChangeHash>>) -> Self {
        Self {
            tracker: heads.map(Tracker::at).unwrap_or_default(),
            state: Rc::new(state),
            demand: Demand::none(),
            revision: 0,
            stale: false,
            uncommitted: None,
            last: Delta::everything(),
            unreadable: None,
        }
    }

    /// The current state. Cheap: shares the value.
    ///
    /// Holding the returned `Rc` across the next [`sync`](Self::sync) makes
    /// that sync read the whole resource again (it cannot update a state
    /// someone else is looking at), so views read through the reader rather
    /// than keep it.
    pub fn state(&self) -> &Rc<R::State> {
        &self.state
    }

    /// What the last sync that changed the state applied: the parts of the
    /// document it touched, or [`Delta::everything`] when it read the state
    /// whole (an open transaction, a failed refresh, the first read).
    pub fn last_delta(&self) -> &Delta {
        &self.last
    }

    /// Why the document does not read as the resource since the last sync,
    /// if it does not: the state is then the last one that did read, and a
    /// view shows this instead of it.
    pub fn unreadable(&self) -> Option<&crate::Unreadable> {
        self.unreadable.as_ref()
    }

    /// Increases whenever the state changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether the state is `doc` as it stands: no change arrived since the
    /// last sync.
    pub fn follows(&self, doc: &Doc) -> bool {
        !self.stale
            && self.uncommitted.is_none()
            && heads_of(doc).is_some_and(|heads| self.tracker.heads() == Some(heads.as_slice()))
    }

    /// Count the unchanged state as a new revision, so everything following
    /// it takes it again (a surface dropping an optimistic preview).
    pub fn represent(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// What views currently demand.
    pub fn current_demand(&self) -> &Demand {
        &self.demand
    }

    /// Apply whatever changed in `doc` since the last sync. Returns whether
    /// the state changed.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document no longer holds this
    /// resource; the state is left as it was last read successfully and the
    /// next sync reads it whole.
    pub fn sync(&mut self, doc: &Doc) -> Result<bool, ModelError> {
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
            self.apply(doc, &Delta::everything())?;
            return Ok(true);
        }
        let state = &self.state;
        let delta = self
            .tracker
            .committed_delta_located(doc, &|obj| R::locate(state, obj));
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
        self.apply(doc, &delta)?;
        Ok(true)
    }

    fn apply(&mut self, doc: &Doc, delta: &Delta) -> Result<(), ModelError> {
        let applied = self.apply_delta(doc, delta);
        self.unreadable = applied
            .as_ref()
            .err()
            .map(|error| crate::Unreadable::of::<R>(doc, error));
        applied
    }

    fn apply_delta(&mut self, doc: &Doc, delta: &Delta) -> Result<(), ModelError> {
        self.stale = true;
        let mut applied = delta.clone();
        let state = match Rc::get_mut(&mut self.state) {
            Some(state) if !delta.is_everything() => {
                if R::refresh(state, doc, delta).is_err() {
                    *state = R::read(doc)?;
                    applied = Delta::everything();
                }
                R::load(state, doc, &self.demand)?;
                None
            }
            _ => {
                let mut state = R::read(doc)?;
                R::load(&mut state, doc, &self.demand)?;
                Some(state)
            }
        };
        self.last = applied;
        if let Some(state) = state {
            self.state = Rc::new(state);
        }
        self.stale = false;
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Ask for the parts `demand` names (replacing the previous demand) and
    /// read those not read yet. Returns whether the state changed.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    pub fn demand(&mut self, doc: &Doc, demand: Demand) -> Result<bool, ModelError>
    where
        R::State: Clone,
    {
        if demand == self.demand {
            return Ok(false);
        }
        self.demand = demand;
        R::load(Rc::make_mut(&mut self.state), doc, &self.demand)?;
        self.revision = self.revision.wrapping_add(1);
        Ok(true)
    }
}
