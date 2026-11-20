//! The parts of a caller's replica's chains taken in on a thread of their own.
//!
//! A replica opened anew holds none of its chains' parts, and a tracker or a mailbox keeps
//! dozens of them. Each is read from the workspace (right after the service starts, from its
//! sealed writes) and handed to the app's worker, which loads it: about a second a part, so a
//! tracker of a few thousand issues took half a minute or more. That happened on the thread
//! that serves the workspace, and every request it serves, a listing of another app as much
//! as one of another workspace, waited behind it.
//!
//! The parts are now read and handed on a thread of their own, and the request waits off the
//! serving thread, as one waiting for a part to arrive does ([`super::part_wait`]); the
//! thread answers everything else meanwhile, and a request for the same replica waits too.
//! Once the parts are in, the request is applied again and finds them held.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, mpsc};

use super::catch_up::{HOLD_ROUNDS, chains_untold, hold, hold_parts, read_parts};
use super::part_wait::{ChainWait, PART_WAIT};
use super::{DocumentRoute, Mounted, Worker, WorkspaceState};
use crate::worker::ConfinedReplica;

/// How many parts a replica holds none of before they are taken in off the serving thread:
/// fewer cost little enough to take in where they are asked for.
const LOADED_APART: usize = 4;

/// What a replica's parts came to once taken in: the parts it holds, with the heads each was
/// handed at, and those of another document's chain.
struct Loaded {
    parts: HashMap<DocumentRoute, Option<Vec<[u8; 32]>>>,
    foreign: HashSet<DocumentRoute>,
}

/// One replica's parts being taken in.
struct Loading {
    app: Arc<ConfinedReplica>,
    done: mpsc::Receiver<Loaded>,
}

/// The replicas whose parts are being taken in off the serving thread, and those that took
/// them in so: each does once, and what it wants after is taken in where it is asked (a chain
/// whose parts its view never reads stays unheld, and is no reason to take them in again).
#[derive(Default)]
pub(super) struct PartLoads {
    loading: Vec<Loading>,
    taken: Vec<std::sync::Weak<ConfinedReplica>>,
}

/// Where `app`'s parts stand.
enum Load {
    /// Nothing is being taken in for it.
    Idle,
    /// They are being taken in.
    Running,
    /// They were taken in, and are merged into the replica now.
    Done(Loaded),
}

impl PartLoads {
    fn of(&mut self, app: &Arc<ConfinedReplica>) -> Load {
        // A replica let go of (evicted, or opened again) takes nothing from what was read
        // for it.
        self.loading
            .retain(|loading| Arc::strong_count(&loading.app) > 1);
        let Some((at, loading)) = self
            .loading
            .iter()
            .enumerate()
            .find(|(_, loading)| Arc::ptr_eq(&loading.app, app))
        else {
            return Load::Idle;
        };
        match loading.done.try_recv() {
            Ok(loaded) => {
                self.loading.swap_remove(at);
                Load::Done(loaded)
            }
            Err(mpsc::TryRecvError::Empty) => Load::Running,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.loading.swap_remove(at);
                Load::Idle
            }
        }
    }

    /// Wait here for `app`'s parts.
    fn wait(&mut self, app: &Arc<ConfinedReplica>) -> Option<Loaded> {
        let at = self
            .loading
            .iter()
            .position(|loading| Arc::ptr_eq(&loading.app, app))?;
        let loading = self.loading.swap_remove(at);
        loading.done.recv().ok()
    }
}

impl Loaded {
    fn merge_into(self, mounted: &mut Mounted) {
        for (route, heads) in self.parts {
            mounted.parts.entry(route).or_insert(heads);
        }
        mounted.foreign.extend(self.foreign);
        // What it wants is asked again where it is used: a part on its way is asked of the
        // workspace from there.
        mounted.wanted_at = None;
    }
}

/// How many parts of `holder`'s chains the workspace lists that `mounted` holds none of.
fn unheld_parts(holder: &DocumentRoute, mounted: &Mounted, state: &WorkspaceState) -> usize {
    state
        .documents
        .iter()
        .filter(|route| route.app_id == holder.app_id && route.cluster_id == holder.cluster_id)
        .filter(|route| super::is_part(&route.document))
        .filter(|route| !mounted.parts.contains_key(*route) && !mounted.foreign.contains(*route))
        .count()
}

/// Take in what `mounted` (a copy holding the parts the replica holds) wants of `holder`'s
/// chains, as [`super::Worker::hold_chains`] does where it waits.
fn take_in(holder: &DocumentRoute, mut mounted: Mounted, state: &WorkspaceState) -> Loaded {
    // A part not here yet is asked for where the request is applied again.
    let _missing = hold_parts(holder, &mut mounted, state);
    for _ in 0..HOLD_ROUNDS {
        let (listed, _) = chains_untold(holder, &mounted, state);
        if listed.is_empty() {
            break;
        }
        for (route, saved) in read_parts(listed, state) {
            hold(route, saved, &mut mounted);
        }
    }
    Loaded {
        parts: mounted.parts,
        foreign: mounted.foreign,
    }
}

impl Worker {
    /// Take up what was taken in of the parts of the caller's replica `key` off this thread:
    /// whether the request being served waits, held off this thread, while they still are.
    /// Nothing else touches the replica while they are: its worker answers one call at a time,
    /// and this thread waited for each part.
    pub(super) fn parts_taken_in(&mut self, key: &super::bounded::BoundedKey) -> bool {
        let Some(app) = self.bounded.get(key).map(|held| Arc::clone(&held.held.app)) else {
            return false;
        };
        let loaded = match self.part_loads.of(&app) {
            Load::Idle => return false,
            Load::Done(loaded) => Some(loaded),
            Load::Running => {
                if matches!(self.part_waits.chain_wait(PART_WAIT), ChainWait::Held) {
                    return self.hold_for_chains();
                }
                // A request that cannot wait off the thread waits here, as it did.
                self.part_loads.wait(&app)
            }
        };
        if let Some(loaded) = loaded
            && let Some(held) = self.bounded.get_mut(key)
        {
            loaded.merge_into(&mut held.held);
        }
        false
    }

    /// Have the caller's replica `key` take in its parts off this thread when it holds none of
    /// many: whether the request being served waits for them, held off this thread. One that
    /// holds most of them, or a request that cannot wait off the thread, takes them in where
    /// it is asked ([`Self::hold_chains`]).
    pub(super) fn load_parts_apart(
        &mut self,
        key: &super::bounded::BoundedKey,
        state: &WorkspaceState,
    ) -> bool {
        let holder = &key.0;
        let Some(held) = self.bounded.get(key) else {
            return false;
        };
        let taken = self
            .part_loads
            .taken
            .iter()
            .any(|taken| std::ptr::eq(taken.as_ptr(), Arc::as_ptr(&held.held.app)));
        if taken
            || unheld_parts(holder, &held.held, state) < LOADED_APART
            || !self.part_waits.may_wait(PART_WAIT)
        {
            return false;
        }
        let app = Arc::clone(&held.held.app);
        let copy = Mounted {
            app: Arc::clone(&app),
            merged: None,
            parts: held.held.parts.clone(),
            foreign: held.held.foreign.clone(),
            wanted_at: None,
            behind: None,
            used: 0,
        };
        let (send, done) = mpsc::channel();
        let holder = holder.clone();
        let state = state.clone();
        let spawned = std::thread::Builder::new()
            .name("parts-in".into())
            .spawn(move || {
                let _ = send.send(take_in(&holder, copy, &state));
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "workspace actions: no thread was started to take in a document's parts");
            return false;
        }
        self.part_loads
            .taken
            .retain(|taken| taken.strong_count() > 0);
        self.part_loads.taken.push(Arc::downgrade(&app));
        self.part_loads.loading.push(Loading { app, done });
        matches!(self.part_waits.chain_wait(PART_WAIT), ChainWait::Held) && self.hold_for_chains()
    }
}
