//! The resource state views read, kept current as the document changes.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_resource::{Delta, Demand, Doc, ModelError, Place, Reader, Resource};

/// A shared handle to the current state of one resource.
///
/// The framework brings the state up to date when the document's heads move
/// — a local action, a collaborator's edit, an undo — reading only what the
/// change touched ([`Resource::refresh`]), and bumps the revision so bound
/// widgets refresh. Views never write it: edits are actions.
///
/// A view of a large resource says what it shows with
/// [`demand`](Self::demand), and only that is read
/// ([`Resource::load`]).
pub struct Live<R: Resource> {
    inner: Rc<RefCell<Reader<R>>>,
    changes: Rc<RefCell<Changes>>,
    places: Rc<RefCell<Places<R>>>,
}

/// Where each participant's last action left them ([`Resource::lands`]):
/// the framework's record of where an agent or a collaborator works, which
/// presence draws and a surface reads instead of keeping its own copy.
struct Places<R: Resource> {
    /// Applied for a participant, not yet placed: placed once the state
    /// holds what they did.
    applied: Vec<(tilo_actions::ParticipantId, R::Action)>,
    at: std::collections::HashMap<tilo_actions::ParticipantId, Place>,
    /// The latest landings in order, numbered, for surfaces to follow.
    log: std::collections::VecDeque<(u64, tilo_actions::ParticipantId, Place)>,
    next: u64,
}

impl<R: Resource> Default for Places<R> {
    fn default() -> Self {
        Self {
            applied: Vec::new(),
            at: std::collections::HashMap::new(),
            log: std::collections::VecDeque::new(),
            next: 0,
        }
    }
}

/// How many landings a surface that missed some can still catch up on.
const LANDINGS_KEPT: usize = 64;

/// What changed since bound controls last followed the state: `None` when
/// nothing did.
#[derive(Default)]
struct Changes {
    pending: Option<Delta>,
}

impl Changes {
    fn note(&mut self, delta: Delta) {
        match &mut self.pending {
            Some(pending) => pending.merge(delta),
            None => self.pending = Some(delta),
        }
    }
}

impl<R: Resource> Clone for Live<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
            changes: Rc::clone(&self.changes),
            places: Rc::clone(&self.places),
        }
    }
}

impl<R: Resource> Live<R> {
    pub(crate) fn new(reader: Reader<R>) -> Self {
        Self {
            inner: Rc::new(RefCell::new(reader)),
            changes: Rc::default(),
            places: Rc::default(),
        }
    }

    /// Note that `action` was applied for `participant`; where it left them
    /// is read once the state follows it ([`Self::place_landings`]).
    pub(crate) fn note_applied(&self, participant: tilo_actions::ParticipantId, action: R::Action) {
        self.places.borrow_mut().applied.push((participant, action));
    }

    /// Place every participant whose action the state now holds, where
    /// [`Resource::lands`] says it left them.
    pub(crate) fn place_landings(&self) {
        let applied = std::mem::take(&mut self.places.borrow_mut().applied);
        if applied.is_empty() {
            return;
        }
        let state = self.get();
        for (participant, action) in applied {
            if let Some(place) = R::lands(&state, &action) {
                self.land(participant, place);
            }
        }
    }

    /// Place `participant` at `place`: where an effect they told where to
    /// act acted ([`EffectCx::place`](super::EffectCx::place)).
    pub(crate) fn land(&self, participant: tilo_actions::ParticipantId, place: Place) {
        let mut places = self.places.borrow_mut();
        let number = places.next;
        places.next = number.saturating_add(1);
        places.at.insert(participant.clone(), place.clone());
        places.log.push_back((number, participant, place));
        if places.log.len() > LANDINGS_KEPT {
            places.log.pop_front();
        }
    }

    /// Where `participant`'s last action left them, as the resource places
    /// it ([`Resource::lands`]): the paragraph an agent appended, the table
    /// it inserted. `None` before they did anything that lands somewhere.
    pub fn place_of(&self, participant: &tilo_actions::ParticipantId) -> Option<Place> {
        self.places.borrow().at.get(participant).cloned()
    }

    /// The landings numbered `from` on, and the number to ask from next.
    pub(crate) fn landings_since(
        &self,
        from: u64,
    ) -> (Vec<(tilo_actions::ParticipantId, Place)>, u64) {
        let places = self.places.borrow();
        let landed = places
            .log
            .iter()
            .filter(|(number, _, _)| *number >= from)
            .map(|(_, participant, place)| (participant.clone(), place.clone()))
            .collect();
        (landed, places.next)
    }

    /// What changed since the last call, for the readers bound to the
    /// state: `None` when nothing did.
    pub(crate) fn take_change(&self) -> Option<Delta> {
        self.changes.borrow_mut().pending.take()
    }

    /// The current state. Cheap: shares the materialised value.
    ///
    /// Keep it only as long as a frame needs it. A state still shared when
    /// the document next changes cannot be updated in place, so the
    /// framework reads the whole resource again instead; read through
    /// [`with`](Self::with) or call this again when you need it.
    pub fn get(&self) -> Rc<R::State> {
        Rc::clone(self.inner.borrow().state())
    }

    /// Read the current state.
    pub fn with<T>(&self, read: impl FnOnce(&R::State) -> T) -> T {
        read(self.inner.borrow().state())
    }

    /// Why the document stopped reading as the resource, if it did: the
    /// state is then the last one that read, and views show this instead.
    pub fn unreadable(&self) -> Option<tilo_resource::Unreadable> {
        self.inner.borrow().unreadable().cloned()
    }

    /// Increases whenever the state changes.
    pub fn revision(&self) -> u64 {
        self.inner.borrow().revision()
    }

    /// Whether the state is `doc` as it stands: no change arrived since the
    /// framework last synced it.
    pub fn follows(&self, doc: &Doc) -> bool {
        self.inner.borrow().follows(doc)
    }

    /// Show the parts of a large resource `demand` names, reading those not
    /// read yet from `doc` — the document the view was synced with, which in
    /// History mode is the past version on screen. Returns whether the state
    /// changed.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold the resource.
    pub fn demand(&self, doc: &Doc, demand: Demand) -> Result<bool, ModelError>
    where
        R::State: Clone,
    {
        let changed = self.inner.borrow_mut().demand(doc, demand)?;
        if changed {
            self.changes.borrow_mut().note(Delta::everything());
        }
        Ok(changed)
    }

    /// Ask everything bound to the state to take it again, unchanged. After
    /// an edit was refused or is waiting for confirmation, a surface that
    /// previewed it optimistically is re-presented the stored state.
    pub(crate) fn represent(&self) {
        self.inner.borrow_mut().represent();
        self.changes.borrow_mut().note(Delta::everything());
    }

    /// What views currently demand.
    pub fn current_demand(&self) -> Demand {
        self.inner.borrow().current_demand().clone()
    }

    /// Apply what changed in `doc` since the last sync, reading only what
    /// changed. The framework does this before every frame; a surface calls
    /// it after an immediate edit ([`Dispatcher::apply_now`](super::Dispatcher::apply_now))
    /// to read its own result within the same event. Returns whether the
    /// state changed.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document no longer holds the resource.
    pub fn sync(&self, doc: &Doc) -> Result<bool, ModelError> {
        #[cfg(feature = "test-support")]
        let _profile = super::profiling::Scope::start("framework.live_sync");
        let mut reader = self.inner.borrow_mut();
        let moved = reader.sync(doc)?;
        if moved {
            self.changes.borrow_mut().note(reader.last_delta().clone());
        }
        Ok(moved)
    }
}
