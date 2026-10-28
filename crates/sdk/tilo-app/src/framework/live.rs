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
    landed: Landings,
    /// Applied here, not yet told: told once the state holds them.
    applied_here: Vec<R::Action>,
    /// The last [`APPLIED_KEPT`] actions applied here the state holds, for
    /// the surfaces that follow them ([`Live::follow_applied`]), oldest
    /// first: the last is numbered `next_here - 1`. A `Vec` like
    /// `applied_here`, so every app compiles one collection of its actions
    /// for this, not two.
    here: Vec<R::Action>,
    /// The number the next action applied here takes.
    next_here: u64,
    /// How many surfaces follow the actions applied here: none, and they are
    /// not kept.
    following_here: usize,
}

impl<R: Resource> Default for Places<R> {
    fn default() -> Self {
        Self {
            applied: Vec::new(),
            landed: Landings::new(),
            applied_here: Vec::new(),
            here: Vec::new(),
            next_here: 0,
            following_here: 0,
        }
    }
}

/// Where each participant was placed, and the latest placings in order.
/// Apart from [`Places`] because it does not name the resource type, so its
/// bookkeeping is compiled once rather than for every resource.
#[derive(Default)]
struct Landings {
    at: std::collections::HashMap<tilo_actions::ParticipantId, Place>,
    /// The latest landings in order, numbered, for surfaces to follow.
    log: std::collections::VecDeque<(u64, tilo_actions::ParticipantId, Place)>,
    next: u64,
}

impl Landings {
    /// Nobody placed yet. Out of line, so every app's [`Live`] calls the
    /// one copy in the runtime.
    #[inline(never)]
    fn new() -> Self {
        Self::default()
    }

    #[inline(never)]
    fn land(&mut self, participant: tilo_actions::ParticipantId, place: Place) {
        let number = self.next;
        self.next = number.saturating_add(1);
        self.at.insert(participant.clone(), place.clone());
        self.log.push_back((number, participant, place));
        if self.log.len() > LANDINGS_KEPT {
            self.log.pop_front();
        }
    }

    #[inline(never)]
    fn since(&self, from: u64) -> (Vec<(tilo_actions::ParticipantId, Place)>, u64) {
        let landed = self
            .log
            .iter()
            .filter(|(number, _, _)| *number >= from)
            .map(|(_, participant, place)| (participant.clone(), place.clone()))
            .collect();
        (landed, self.next)
    }
}

/// How many landings a surface that missed some can still catch up on.
const LANDINGS_KEPT: usize = 64;

/// How many actions applied here a surface that missed some can still catch
/// up on: more than one event's group applies at once.
const APPLIED_KEPT: usize = 1024;

/// What changed since bound controls last followed the state: `None` when
/// nothing did.
#[derive(Default)]
struct Changes {
    pending: Option<Delta>,
}

impl Changes {
    /// None yet, shared. Out of line, so every app's [`Live`] calls the one
    /// copy in the runtime.
    #[inline(never)]
    fn shared() -> Rc<RefCell<Self>> {
        Rc::default()
    }

    fn note(&mut self, delta: Delta) {
        match &mut self.pending {
            Some(pending) => pending.merge(delta),
            None => self.pending = Some(delta),
        }
    }
}

/// Note in `changes` what a sync applied, `delta`. Out of line, so every
/// app's [`Live::sync`] calls the one copy in the runtime.
#[inline(never)]
fn note_synced(changes: &RefCell<Changes>, delta: &Delta) {
    changes.borrow_mut().note(delta.clone());
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
            changes: Changes::shared(),
            places: Rc::default(),
        }
    }

    /// Note that `action` was applied for `participant`; where it left them
    /// is read once the state follows it ([`Self::place_landings`]).
    pub(crate) fn note_applied(&self, participant: tilo_actions::ParticipantId, action: R::Action) {
        self.places.borrow_mut().applied.push((participant, action));
    }

    /// Whether a surface follows the actions applied here
    /// ([`Self::follow_applied`]): none does, and they are not noted.
    pub(crate) fn follows_applied(&self) -> bool {
        self.places.borrow().following_here > 0
    }

    /// A surface follows the actions applied here from now on, asking from
    /// the number this answers ([`Self::applied_since`]).
    pub(crate) fn follow_applied(&self) -> u64 {
        let mut places = self.places.borrow_mut();
        places.following_here = places.following_here.saturating_add(1);
        places.next_here
    }

    /// A surface that followed the actions applied here stopped: when none
    /// is left, none are kept.
    pub(crate) fn unfollow_applied(&self) {
        let mut places = self.places.borrow_mut();
        places.following_here = places.following_here.saturating_sub(1);
        if places.following_here == 0 {
            places.applied_here.clear();
            places.here.clear();
        }
    }

    /// Note that `action` was applied here, through any door; surfaces that
    /// follow it are told once the state holds it ([`Self::place_landings`]).
    pub(crate) fn note_applied_here(&self, action: R::Action) {
        self.places.borrow_mut().applied_here.push(action);
    }

    /// The actions applied here the state holds, numbered `from` on, and the
    /// number to ask from next.
    pub(crate) fn applied_since(&self, from: u64) -> (Vec<R::Action>, u64) {
        let places = self.places.borrow();
        let kept = u64::try_from(places.here.len()).unwrap_or(u64::MAX);
        let first = places.next_here.saturating_sub(kept);
        let skip = usize::try_from(from.saturating_sub(first)).unwrap_or(usize::MAX);
        let applied = places.here.iter().skip(skip).cloned().collect();
        (applied, places.next_here)
    }

    /// Place every participant whose action the state now holds, where
    /// [`Resource::lands`] says it left them, and hold the actions applied
    /// here for the surfaces that follow them.
    #[inline(never)]
    pub(crate) fn place_landings(&self) {
        {
            let mut places = self.places.borrow_mut();
            let mut here = std::mem::take(&mut places.applied_here);
            let landed = u64::try_from(here.len()).unwrap_or(u64::MAX);
            places.next_here = places.next_here.saturating_add(landed);
            places.here.append(&mut here);
            let excess = places.here.len().saturating_sub(APPLIED_KEPT);
            if excess > 0 {
                places.here = places.here.split_off(excess);
            }
        }
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
        self.places.borrow_mut().landed.land(participant, place);
    }

    /// Where `participant`'s last action left them, as the resource places
    /// it ([`Resource::lands`]): the paragraph an agent appended, the table
    /// it inserted. `None` before they did anything that lands somewhere.
    pub fn place_of(&self, participant: &tilo_actions::ParticipantId) -> Option<Place> {
        self.places.borrow().landed.at.get(participant).cloned()
    }

    /// The landings numbered `from` on, and the number to ask from next.
    pub(crate) fn landings_since(
        &self,
        from: u64,
    ) -> (Vec<(tilo_actions::ParticipantId, Place)>, u64) {
        self.places.borrow().landed.since(from)
    }

    /// The number the next landing will have: where a follower that has
    /// seen every landing so far asks from.
    pub(crate) fn next_landing(&self) -> u64 {
        self.places.borrow().landed.next
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

    /// The current state, when the document reads as the resource: `None`
    /// while it does not, so a caller publishing what the state says never
    /// reports the last state that read as if it were what the document
    /// holds.
    pub fn get_checked(&self) -> Option<Rc<R::State>> {
        let inner = self.inner.borrow();
        inner
            .unreadable()
            .is_none()
            .then(|| Rc::clone(inner.state()))
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

    /// Increases whenever what the state says changes. A view reading
    /// more of the same document ([`Self::demand`]) is no new revision.
    pub fn revision(&self) -> u64 {
        self.inner.borrow().revision()
    }

    /// Increases whenever a demand read parts of the document the state did
    /// not hold yet: what a view of a large resource holds moved, not what
    /// the resource says.
    pub fn loads(&self) -> u64 {
        self.inner.borrow().loads()
    }

    /// Whether the state is `doc` as it stands: no change arrived since the
    /// framework last synced it.
    pub fn follows(&self, doc: &Doc) -> bool {
        self.inner.borrow().follows(doc)
    }

    /// Show the parts of a large resource `demand` names, reading those not
    /// read yet from `doc` — the document the view was synced with, which in
    /// History mode is the past version on screen. Returns whether the state
    /// read more ([`Self::loads`]). What the resource says is unchanged, so
    /// this is no revision and no change to follow.
    #[inline(never)]
    pub fn demand(&self, doc: &Doc, demand: Demand) -> Result<bool, ModelError>
    where
        R::State: Clone,
    {
        self.inner.borrow_mut().demand(doc, demand)
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
    #[inline(never)]
    pub fn sync(&self, doc: &Doc) -> Result<bool, ModelError> {
        let _profile = tilo_resource::profiling::Phase::start("framework.live_sync");
        let mut reader = self.inner.borrow_mut();
        let moved = reader.sync(doc)?;
        if moved {
            note_synced(&self.changes, reader.last_delta());
        }
        Ok(moved)
    }

    /// Read the state whole from `doc`, whatever the last sync said, so a
    /// change this reader cannot apply whole is noticed and reported: for a
    /// resource kept small enough to read that way, which nothing else
    /// would find out about. Returns whether the state changed.
    pub fn reread(&self, doc: &Doc) -> Result<bool, ModelError> {
        let mut reader = self.inner.borrow_mut();
        let moved = reader.reread_whole(doc)?;
        if moved {
            note_synced(&self.changes, reader.last_delta());
        }
        Ok(moved)
    }
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
