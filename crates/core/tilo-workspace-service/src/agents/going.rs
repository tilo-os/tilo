//! The turns going on this host, and what reaches one while it works.
//!
//! An agent has one turn at a time in a conversation. While it goes, two things reach it, both
//! read from the workspace's records like every ask: its person's stop ([`Going::stop`]), which
//! ends it at its next step and ends a model call under way; and what its person wrote since
//! ([`Going::reach`]), which the turn is told at its next step and goes on with, so their
//! newer words neither start a second answer beside the first nor throw its work away. What a
//! cause asks (a reminder, an event) never reaches a turn that is going: it waits its turn.
//!
//! Whoever hands a turn something read the workspace first, and the turn may have moved since:
//! it began only after, or went on with newer words. What was read before that can be older
//! than what the turn answers now, and a turn that took it would leave its person's newer
//! message for an older one. So a look takes a [`Reading`] before it reads, and a turn that
//! moved since takes nothing of it ([`Going::reach_read`]): the look reads again. And no turn
//! takes twice what it answered or left behind.
//!
//! This is the one thing the host keeps of a turn besides the records, and only while it runs:
//! a host that restarts holds none. A turn's stop is the cancel of the `#answer` invocation that
//! started it ([`Turns::enter_stopping`]): Chat's Stop cancels the invocation its reply names, and
//! the flag the cancel sets is the turn's own.
//!
//! A server that is stopping closes its turns ([`Turns::close`]): no turn begins any more, and
//! it waits until the turns going ended ([`Turns::idle`]) before it stops, so a restart never
//! cuts a turn off halfway. What was asked meanwhile stays in the workspace, and the next start
//! answers it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

use super::{Ask, Turn};

/// Where an agent has one turn at a time: the workspace, the agent, and the conversation, or
/// for a task the task itself.
pub(super) type Place = (String, String, String);

pub(super) fn place(workspace: &str, turn: &Turn) -> Place {
    let at = match &turn.ask {
        // Each task is worked on in sessions of its own, whatever else its agent works on.
        Ask::Triggered(place) if place.app == crate::agent_shortcuts::TASKS => {
            format!("{}/{}", place.app, place.item)
        }
        Ask::Triggered(_) => turn.conversation.clone(),
    };
    (workspace.to_owned(), turn.agent.id.0.clone(), at)
}

/// One turn going.
pub(super) struct Going {
    /// The ask it answers now: it moves on when its person writes again.
    ask: Mutex<Ask>,
    /// The person it works for.
    person: String,
    /// Set by that person's stop: the cancel of the invocation that began the turn.
    stop: Arc<AtomicBool>,
    /// The stops of the invocations whose asks it went on with: a cancel of any of them stops it
    /// at its next step.
    stops: Mutex<Vec<Arc<AtomicBool>>>,
    /// Set once its place is given back, for whoever waits until it ended ([`Self::ended`]).
    over: AtomicBool,
    ended: tokio::sync::Notify,
    /// What the person wrote since it began and it has not been told yet, oldest first.
    came: Mutex<Vec<Turn>>,
    /// Every ask it answered so far, the one it answers now included: none reaches it again.
    had: Mutex<Vec<Ask>>,
    /// The server's count of the times a turn began or went on ([`Turns::reading`]).
    moves: Arc<AtomicU64>,
    /// That count as this turn's beginning, or its last going on, left it.
    moved: AtomicU64,
}

/// Where a server's turns stood when a look began to read ([`Turns::reading`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct Reading(u64);

/// What became of a turn handed to one that goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Handed {
    /// The turn that goes has it, and goes on with it at its next step.
    Taken,
    /// It is nothing that turn takes.
    Left,
    /// It was read before that turn began or last went on, so it may be older than what the
    /// turn answers now: whoever read it reads again.
    Stale,
}

impl Going {
    /// The ask it answers now.
    #[cfg(test)]
    pub(super) fn ask(&self) -> Ask {
        self.ask.lock_recover().clone()
    }

    pub(super) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
            || self
                .stops
                .lock_recover()
                .iter()
                .any(|stop| stop.load(Ordering::Relaxed))
    }

    /// Stop it, too, when `stop` is set: the cancel of an invocation whose ask it went on with.
    pub(super) fn stopped_by(&self, stop: Arc<AtomicBool>) {
        self.stops.lock_recover().push(stop);
    }

    /// Wait until it ended: its place was given back.
    pub(super) async fn ended(&self) {
        loop {
            let ending = self.ended.notified();
            if self.over.load(Ordering::SeqCst) {
                return;
            }
            ending.await;
        }
    }

    /// The stop itself, for a model call under way to end on.
    pub(super) fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Hand it `turn`, which the records give in its place now: whether it takes it. It takes
    /// a message the person it works for wrote themselves that it has not been handed, and
    /// nothing else: what a cause asks, and an ask for somebody else, wait until this turn ended.
    /// Whoever hands a turn something read the workspace first and says when
    /// ([`Self::reach_read`]): this is the hand-over itself, for the tests of it.
    #[cfg(test)]
    pub(super) fn reach(&self, turn: Turn) -> bool {
        self.hand(turn, None) == Handed::Taken
    }

    /// [`Self::reach`], for a turn a look read at `reading`: it is taken only when this turn
    /// neither began nor went on since.
    pub(super) fn reach_read(&self, turn: Turn, reading: Reading) -> Handed {
        self.hand(turn, Some(reading))
    }

    fn hand(&self, turn: Turn, reading: Option<Reading>) -> Handed {
        let in_conversation =
            matches!(&turn.ask, Ask::Triggered(place) if place.app == crate::agent_shortcuts::CHAT);
        if !turn.typed || turn.person != self.person || !in_conversation {
            return Handed::Left;
        }
        // Held from here on, as `take` holds it while the turn goes on: what is handed is
        // judged against the turn as it stands, never against one half moved.
        let mut came = self.came.lock_recover();
        if self.had.lock_recover().contains(&turn.ask)
            || came.iter().any(|held| held.ask == turn.ask)
        {
            return Handed::Left;
        }
        if reading.is_some_and(|reading| self.moved.load(Ordering::SeqCst) > reading.0) {
            return Handed::Stale;
        }
        came.push(turn);
        Handed::Taken
    }

    /// Whether something reached it that it has not taken.
    pub(super) fn waiting(&self) -> bool {
        !self.came.lock_recover().is_empty()
    }

    /// Everything that reached it since it last took any, oldest first; the newest is the ask
    /// it answers from now on.
    pub(super) fn take(&self) -> Vec<Turn> {
        // Held until the turn has moved whole: nothing is handed to one half gone on.
        let mut came = self.came.lock_recover();
        if let Some(newest) = came.last() {
            *self.ask.lock_recover() = newest.ask.clone();
            self.had
                .lock_recover()
                .extend(came.iter().map(|turn| turn.ask.clone()));
            // It goes on: what was read before this may be older than what it answers now.
            let moved = self.moves.fetch_add(1, Ordering::SeqCst).saturating_add(1);
            self.moved.store(moved, Ordering::SeqCst);
        }
        std::mem::take(&mut *came)
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
        drop(going);
        self.going.over.store(true, Ordering::SeqCst);
        self.going.ended.notify_waiters();
    }
}

/// What entering a turn's place found.
pub(super) enum Entered {
    /// Nothing went there: the turn begins, and holds the place.
    Began(Held),
    /// A turn goes there already.
    Goes(Arc<Going>),
    /// The server is stopping: no turn begins.
    Closed,
}

/// The turns going on a host.
#[derive(Default)]
pub(super) struct Turns {
    going: Mutex<BTreeMap<Place, Arc<Going>>>,
    /// How often a turn of this server began, or went on with what its person wrote since.
    moves: Arc<AtomicU64>,
    /// Set once the server stops: no turn begins any more.
    closed: AtomicBool,
}

impl Turns {
    /// Where this server's turns stand, for a look to take before it reads: a turn that
    /// begins or goes on after it takes nothing that look read ([`Going::reach_read`]).
    pub(super) fn reading(&self) -> Reading {
        Reading(self.moves.load(Ordering::SeqCst))
    }

    /// Begin no turn from now on: the server is stopping.
    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// How many turns go now.
    pub(super) fn going(&self) -> usize {
        self.going.lock_recover().len()
    }

    /// Whether no turn goes.
    pub(super) fn idle(&self) -> bool {
        self.going() == 0
    }

    /// Begin `turn` in `workspace`, unless a turn of its agent goes in its place already.
    #[cfg(test)]
    pub(super) fn enter(self: &Arc<Self>, workspace: &str, turn: &Turn) -> Entered {
        self.enter_stopping(workspace, turn, Arc::default())
    }

    /// [`Self::enter`] for a turn that `stop` stops: the flag the cancel of the invocation that
    /// began it sets.
    pub(super) fn enter_stopping(
        self: &Arc<Self>,
        workspace: &str,
        turn: &Turn,
        stop: Arc<AtomicBool>,
    ) -> Entered {
        let place = place(workspace, turn);
        let mut going = self.going.lock_recover();
        if let Some(held) = going.get(&place) {
            return Entered::Goes(Arc::clone(held));
        }
        if self.closed.load(Ordering::SeqCst) {
            return Entered::Closed;
        }
        let began = Arc::new(Going {
            ask: Mutex::new(turn.ask.clone()),
            person: turn.person.clone(),
            stop,
            stops: Mutex::default(),
            over: AtomicBool::new(false),
            ended: tokio::sync::Notify::new(),
            came: Mutex::default(),
            had: Mutex::new(vec![turn.ask.clone()]),
            moved: AtomicU64::new(self.moves.fetch_add(1, Ordering::SeqCst).saturating_add(1)),
            moves: Arc::clone(&self.moves),
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

#[cfg(test)]
pub(super) mod tests {
    #![allow(
        clippy::panic,
        reason = "in a test this is the assertion, not a hazard"
    )]

    use tilo_workspace_agent::Agent;

    use super::*;

    /// What `ada` wrote her agent as `message`, in their own conversation.
    pub(in crate::agents) fn wrote(message: &str) -> Turn {
        Turn {
            agent: Agent {
                id: tilo_resource::Atom("scout".to_owned()),
                ..Agent::default()
            },
            person: "ada".to_owned(),
            ask: Ask::Triggered(crate::work::Where {
                app: crate::agent_shortcuts::CHAT.to_owned(),
                kind: crate::agent_shortcuts::MESSAGE.to_owned(),
                record: "ada with scout".to_owned(),
                item: message.to_owned(),
                change: message.to_owned(),
            }),
            conversation: "ada with scout".to_owned(),
            cause: message.to_owned(),
            request: message.to_owned(),
            history: Vec::new(),
            asked: message.to_owned(),
            invocation: None,
            typed: true,
            saved_by: None,
            private: false,
            line: None,
            task: None,
        }
    }

    /// The turn that begins on `turn`, in a place that is free.
    fn begun(turns: &Arc<Turns>, turn: &Turn) -> Held {
        match turns.enter("w", turn) {
            Entered::Began(held) => held,
            Entered::Goes(_) | Entered::Closed => panic!("a turn goes in its place already"),
        }
    }

    /// The turn that goes where `turn` would begin.
    fn goes(turns: &Arc<Turns>, turn: &Turn) -> Arc<Going> {
        match turns.enter("w", turn) {
            Entered::Goes(going) => going,
            Entered::Began(_) | Entered::Closed => panic!("no turn goes in its place"),
        }
    }

    /// A stopping server begins no turn, still hands a going one what its person wrote, and is
    /// idle once that turn ended.
    #[test]
    fn a_closed_server_begins_no_turn_and_waits_for_the_one_going() {
        let turns = Arc::new(Turns::default());
        let held = begun(&turns, &wrote("first"));
        turns.close();
        assert!(!turns.idle());
        goes(&turns, &wrote("second"));
        let mut other = wrote("elsewhere");
        other.conversation = "ada alone".to_owned();
        assert!(matches!(turns.enter("w", &other), Entered::Closed));
        drop(held);
        assert!(turns.idle());
        assert!(matches!(turns.enter("w", &wrote("third")), Entered::Closed));
    }

    /// A turn that went on with what its person wrote next is not handed the message it left:
    /// a look that read while it still answered that one would have it go back to it.
    #[test]
    fn a_turn_that_went_on_is_not_handed_the_message_it_left() {
        let turns = Arc::new(Turns::default());
        let held = begun(&turns, &wrote("first"));
        assert!(
            held.reach(wrote("second")),
            "what its person wrote since reaches it"
        );
        assert_eq!(held.take().len(), 1);
        assert_eq!(
            held.ask(),
            wrote("second").ask,
            "it answers the newer message now"
        );
        assert!(
            !held.reach(wrote("first")),
            "and is not handed the one it left"
        );
        assert!(!held.waiting());
    }

    /// What a look read before a turn began is not handed to that turn, and what was read since
    /// is, once.
    #[test]
    fn a_turn_takes_nothing_read_before_it_began() {
        let turns = Arc::new(Turns::default());
        let before = turns.reading();
        let held = begun(&turns, &wrote("second"));
        assert_eq!(
            held.reach_read(wrote("first"), before),
            Handed::Stale,
            "read when this turn had not begun: it may be older than what the turn answers"
        );
        assert!(!held.waiting());
        assert_eq!(
            held.reach_read(wrote("second"), before),
            Handed::Left,
            "its own ask is nothing to hand it, whenever it was read"
        );
        let since = turns.reading();
        assert_eq!(held.reach_read(wrote("third"), since), Handed::Taken);
        assert_eq!(held.reach_read(wrote("third"), since), Handed::Left);
        assert_eq!(held.take().len(), 1);
    }

    /// What a look read before a turn went on is not handed to it afterwards, and a turn that
    /// took nothing has not gone on.
    #[test]
    fn a_turn_takes_nothing_read_before_it_went_on() {
        let turns = Arc::new(Turns::default());
        let held = begun(&turns, &wrote("first"));
        let early = turns.reading();
        assert!(held.take().is_empty());
        assert_eq!(held.reach_read(wrote("second"), early), Handed::Taken);
        assert_eq!(held.take().len(), 1);
        assert_eq!(
            held.reach_read(wrote("older"), early),
            Handed::Stale,
            "read while it still answered the first message"
        );
        assert_eq!(
            held.reach_read(wrote("third"), turns.reading()),
            Handed::Taken
        );
    }

    /// A look reads while a turn answers, the turn ends, and the next one begins on a newer
    /// message before that look enters the place: the turn that goes there now takes nothing of
    /// what the look read.
    #[test]
    fn the_turn_after_one_that_ended_takes_nothing_read_before_it_began() {
        let turns = Arc::new(Turns::default());
        let first = begun(&turns, &wrote("first"));
        let read = turns.reading();
        drop(first);
        let _second = begun(&turns, &wrote("second"));
        let going = goes(&turns, &wrote("first"));
        assert_eq!(going.reach_read(wrote("first"), read), Handed::Stale);
        assert!(!going.waiting());
    }
}
