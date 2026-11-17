//! The turns going on this host, and what reaches one while it works.
//!
//! An agent has one turn at a time in a conversation. While it goes, two things reach it, both
//! read from the workspace's records like every ask: its person's stop ([`Going::stop`]), which
//! ends it at its next step and ends a model call under way; and what its person wrote since
//! ([`Going::reach`]), which the turn is told at its next step and goes on with, so their
//! newer words neither start a second answer beside the first nor throw its work away. What a
//! cause asks (a reminder, an event) never reaches a turn that is going: it waits its turn.
//!
//! This is the one thing the host keeps of a turn besides the records, and only while it runs:
//! a host that restarts holds none, and reads from the records what was left
//! ([`chat::summons::left_begun`]).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

use super::{Ask, Turn};

/// Where an agent has one turn at a time: the workspace, the agent, and the conversation, or
/// for an issue its ask.
type Place = (String, String, String);

fn place(workspace: &str, turn: &Turn) -> Place {
    let at = match &turn.ask {
        Ask::Message { .. } => turn.conversation.clone(),
        Ask::Issue {
            record,
            issue,
            cause,
        } => format!("{record}\0{issue}\0{cause}"),
    };
    (workspace.to_owned(), turn.agent.id.0.clone(), at)
}

/// One turn going.
pub(super) struct Going {
    /// The ask it answers now: it moves on when its person writes again.
    ask: Mutex<Ask>,
    /// The person it works for.
    person: String,
    /// Set by that person's stop.
    stop: Arc<AtomicBool>,
    /// What the person wrote since it began and it has not been told yet, oldest first.
    came: Mutex<Vec<Turn>>,
}

impl Going {
    /// The ask it answers now.
    pub(super) fn ask(&self) -> Ask {
        self.ask.lock_recover().clone()
    }

    /// Its person stopped it: it ends at its next step.
    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub(super) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// The stop itself, for a model call under way to end on.
    pub(super) fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Hand it `turn`, which the records give in its place now: whether it takes it. It takes
    /// a message the person it works for wrote themselves that it has not been handed, and
    /// nothing else: what a cause asks, and an ask for somebody else, wait until this turn ended.
    pub(super) fn reach(&self, turn: Turn) -> bool {
        if !turn.typed || turn.person != self.person || !matches!(turn.ask, Ask::Message { .. }) {
            return false;
        }
        if *self.ask.lock_recover() == turn.ask {
            return false;
        }
        let mut came = self.came.lock_recover();
        if came.iter().any(|held| held.ask == turn.ask) {
            return false;
        }
        came.push(turn);
        true
    }

    /// Whether something reached it that it has not taken.
    pub(super) fn waiting(&self) -> bool {
        !self.came.lock_recover().is_empty()
    }

    /// Everything that reached it since it last took any, oldest first; the newest is the ask
    /// it answers from now on.
    pub(super) fn take(&self) -> Vec<Turn> {
        let came = std::mem::take(&mut *self.came.lock_recover());
        if let Some(newest) = came.last() {
            *self.ask.lock_recover() = newest.ask.clone();
        }
        came
    }
}

/// A turn's place, held while it goes: dropping it frees the place for the next turn.
pub(super) struct Held {
    turns: Arc<Turns>,
    place: Place,
    going: Arc<Going>,
}

impl std::ops::Deref for Held {
    type Target = Going;

    fn deref(&self) -> &Going {
        &self.going
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut going = self.turns.going.lock_recover();
        if going
            .get(&self.place)
            .is_some_and(|held| Arc::ptr_eq(held, &self.going))
        {
            going.remove(&self.place);
        }
    }
}

/// What entering a turn's place found.
pub(super) enum Entered {
    /// Nothing went there: the turn begins, and holds the place.
    Began(Held),
    /// A turn goes there already.
    Goes(Arc<Going>),
}

/// The turns going on a host.
#[derive(Default)]
pub(super) struct Turns {
    going: Mutex<BTreeMap<Place, Arc<Going>>>,
}

impl Turns {
    /// Begin `turn` in `workspace`, unless a turn of its agent goes in its place already.
    pub(super) fn enter(self: &Arc<Self>, workspace: &str, turn: &Turn) -> Entered {
        let place = place(workspace, turn);
        let mut going = self.going.lock_recover();
        if let Some(held) = going.get(&place) {
            return Entered::Goes(Arc::clone(held));
        }
        let began = Arc::new(Going {
            ask: Mutex::new(turn.ask.clone()),
            person: turn.person.clone(),
            stop: Arc::default(),
            came: Mutex::default(),
        });
        going.insert(place.clone(), Arc::clone(&began));
        drop(going);
        Entered::Began(Held {
            turns: Arc::clone(self),
            place,
            going: began,
        })
    }
}
