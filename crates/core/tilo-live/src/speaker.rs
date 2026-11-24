//! The one place a device says its presence in one workspace: the daemon's, over the live
//! session it holds.
//!
//! What the device says is put together from the parts that know it: the person's shell, while
//! a desktop session runs, says who they are, where they point and the caret in their screen
//! ([`Speaker::set_shell`]); the device's own apps say what they do in their records
//! ([`Speaker::set_app_word`]); a call the device joined says the channel it is in
//! ([`Speaker::set_joined`]); and a bridge says the people it carries in from another service
//! ([`Speaker::set_bridged`]). A device with no session at all still says who it is, from its
//! certificate. The board diffs the whole word against what it said last and publishes only what
//! moved, on its one schedule, with the keepalives that keep the device listed
//! ([`crate::presence::Board::due`]).
//!
//! What everyone else says is heard here too: their words in their apps are handed on to this
//! device's apps as they change ([`Speaker::on_heard`]), and the calls they are in read off the
//! same board ([`Speaker::calls`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use collab_types::MemberId;
use collab_types::presence::{Payload, Presence, PresenceKey, ShellPresence};
use tilo_lock::MutexRecover as _;

use crate::live::Track;
use crate::live_session::{Heard, LiveSession};
use crate::presence::{Board, BridgedMember, Call, LiveCall, Own};

/// How long the schedule waits at most between two looks at what is due: a word set from
/// elsewhere wakes it at once.
const IDLE: Duration = Duration::from_secs(1);

/// What hears every other member's word in an app as it changes: who said it, and the frame.
pub type Heard2Apps = Arc<dyn Fn(&MemberId, &Presence) + Send + Sync>;

/// One member's word in one app under one key, as it was last handed on.
type Word = (MemberId, String, PresenceKey);

/// One workspace's presence as this device says and hears it.
pub struct Speaker {
    session: Arc<LiveSession>,
    state: Mutex<State>,
    moved: tokio::sync::Notify,
    heard: Mutex<Option<Heard2Apps>>,
}

struct State {
    board: Board,
    workspace: String,
    /// What the device says of itself with no person's shell running: its name and its person.
    base: Own,
    shell: Option<Own>,
    /// The runs this device executes, each said as a member of its own:
    /// `(session, member, face)`.
    agents: Vec<(String, String, ShellPresence)>,
    apps: Vec<Presence>,
    joined: Option<Call>,
    bridged: Vec<BridgedMember>,
    /// Every other member's words in their apps, as last handed on.
    handed: HashMap<Word, Payload>,
}

impl State {
    fn own(&self) -> Own {
        let mut own = self.shell.clone().unwrap_or_else(|| self.base.clone());
        own.apps.extend(self.apps.iter().cloned());
        if own.call.is_none() {
            own.call.clone_from(&self.joined);
        }
        if !self.bridged.is_empty() {
            own.bridged.clone_from(&self.bridged);
        }
        own
    }

    /// What every other member said in their apps that moved since it was last handed on, and
    /// each word nobody says any more, as `Left`.
    fn moved_words(&mut self) -> Vec<(MemberId, Presence)> {
        let mut now: HashMap<Word, Payload> = HashMap::new();
        for member in self.board.here() {
            let Some(who) = member
                .member
                .clone()
                .map(MemberId::agent)
                .or_else(|| member.user.clone().map(MemberId::person))
            else {
                continue;
            };
            for (app, key, payload) in &member.apps {
                now.insert((who.clone(), app.clone(), key.clone()), payload.clone());
            }
        }
        let mut moved = Vec::new();
        for ((who, app, key), _) in self
            .handed
            .iter()
            .filter(|(word, _)| !now.contains_key(*word))
        {
            moved.push((
                who.clone(),
                Presence::app(app.clone(), &self.workspace, key.clone(), Payload::Left),
            ));
        }
        for ((who, app, key), payload) in &now {
            if self.handed.get(&(who.clone(), app.clone(), key.clone())) != Some(payload) {
                moved.push((
                    who.clone(),
                    Presence::app(app.clone(), &self.workspace, key.clone(), payload.clone()),
                ));
            }
        }
        self.handed = now;
        moved
    }
}

impl Speaker {
    /// Say `base` in `workspace` on `session` as `device` from now on, and hear everyone else
    /// there. Runs on the current tokio runtime until the speaker or the session goes.
    #[must_use]
    pub fn start(
        session: &Arc<LiveSession>,
        workspace: impl Into<String>,
        device: [u8; 32],
        base: Own,
    ) -> Arc<Self> {
        let workspace = workspace.into();
        let mut board = Board::new(workspace.clone(), device);
        board.say(&base);
        let speaker = Arc::new(Self {
            session: Arc::clone(session),
            state: Mutex::new(State {
                board,
                workspace,
                base,
                shell: None,
                agents: Vec::new(),
                apps: Vec::new(),
                joined: None,
                bridged: Vec::new(),
                handed: HashMap::new(),
            }),
            moved: tokio::sync::Notify::new(),
            heard: Mutex::new(None),
        });
        let weak = Arc::downgrade(&speaker);
        session.tap(
            Track::Presence,
            Arc::new(move |heard: &Heard| {
                weak.upgrade().map(|speaker| speaker.hear(heard)).is_some()
            }),
        );
        tokio::spawn(run(Arc::downgrade(&speaker)));
        speaker
    }

    /// Hand every other member's word in an app to `heard` as it changes.
    pub fn on_heard(&self, heard: Heard2Apps) {
        *self.heard.lock_recover() = Some(heard);
    }

    /// The person's shell says `own` here now, or nothing (`None`): the device says what it is
    /// with no shell running.
    pub fn set_shell(&self, own: Option<Own>) {
        self.change(|state| {
            let moved = state.shell != own;
            state.shell = own;
            moved
        });
    }

    /// Speak for `agents`, the runs this device executes here, each `(session, member, face)`.
    pub fn set_agents(&self, agents: Vec<(String, String, ShellPresence)>) {
        let moved = {
            let mut state = self.state.lock_recover();
            let moved = state.agents != agents;
            if moved {
                state.board.speak_for(&agents);
                state.agents = agents;
            }
            moved
        };
        if moved {
            self.moved.notify_one();
        }
    }

    /// One of this device's apps said `payload` under `key` in its own app `app`: kept as its
    /// last word there, and taken back with `Left`.
    pub fn set_app_word(&self, app: &str, key: PresenceKey, payload: Option<Payload>) {
        self.change(|state| {
            let at = state
                .apps
                .iter()
                .position(|frame| frame.app == app && frame.key == key);
            match (payload, at) {
                (None | Some(Payload::Left), Some(at)) => {
                    state.apps.remove(at);
                    true
                }
                (None | Some(Payload::Left), None) => false,
                (Some(payload), at) => {
                    let frame = Presence::app(app, &state.workspace, key, payload);
                    match at.and_then(|at| state.apps.get_mut(at)) {
                        Some(held) if *held == frame => false,
                        Some(held) => {
                            *held = frame;
                            true
                        }
                        None => {
                            state.apps.push(frame);
                            true
                        }
                    }
                }
            }
        });
    }

    /// The device joined `call`, or left the one it was in (`None`). Said at once, so the live
    /// session holds the device to that call's media from this moment.
    pub fn set_joined(&self, call: Option<Call>) {
        self.change(|state| {
            let moved = state.joined != call;
            state.joined = call;
            moved
        });
        self.say_now();
    }

    /// The call the device says it is in now, as everyone here reads it.
    #[must_use]
    pub fn call(&self) -> Option<Call> {
        self.state.lock_recover().own().call
    }

    /// The whole word this device says in the workspace now, as everyone there reads it.
    #[must_use]
    pub fn word(&self) -> Own {
        self.state.lock_recover().own()
    }

    /// The people this device carries into its call from another service, each a member of it
    /// of their own.
    pub fn set_bridged(&self, bridged: Vec<BridgedMember>) {
        self.change(|state| {
            let moved = state.bridged != bridged;
            state.bridged = bridged;
            moved
        });
    }

    /// The calls in progress here, as those in them say.
    #[must_use]
    pub fn calls(&self) -> Vec<LiveCall> {
        self.state.lock_recover().board.calls()
    }

    /// The live session this speaker says its word on.
    #[must_use]
    pub fn session(&self) -> &Arc<LiveSession> {
        &self.session
    }

    fn change(&self, change: impl FnOnce(&mut State) -> bool) {
        let moved = {
            let mut state = self.state.lock_recover();
            let moved = change(&mut state);
            if moved {
                let own = state.own();
                state.board.say(&own);
            }
            moved
        };
        if moved {
            self.moved.notify_one();
        }
    }

    /// Say everything that is owed now, past the rate ceiling.
    fn say_now(&self) {
        let frames = self.state.lock_recover().board.flush(now_ms());
        self.publish(&frames);
    }

    /// Say what is due now.
    fn pump(&self) -> Option<Duration> {
        let now = now_ms();
        let (frames, next) = {
            let mut state = self.state.lock_recover();
            let frames = state.board.due(now);
            (frames, state.board.next_due(now))
        };
        if let Some(frames) = frames {
            self.publish(&frames);
        }
        next.map(|at| Duration::from_secs_f64(((at - now) / 1_000.0).max(0.0)))
    }

    fn publish(&self, frames: &[(u32, Presence)]) {
        if frames.is_empty() {
            return;
        }
        // A word the session did not carry is not kept: presence is ephemeral, and the next
        // word, or the keepalive, says where the device is again.
        let _ = crate::presence::say(&self.session, frames);
    }

    fn hear(&self, heard: &Heard) {
        let moved = {
            let mut state = self.state.lock_recover();
            if !state.board.heard(heard.sender, &heard.body, now_ms()) {
                return;
            }
            state.moved_words()
        };
        if moved.is_empty() {
            return;
        }
        let hand = self.heard.lock_recover().clone();
        if let Some(hand) = hand {
            for (who, frame) in &moved {
                hand(who, frame);
            }
        }
    }
}

/// Say what `speaker` owes, as it comes due, until the speaker or its session goes.
async fn run(speaker: Weak<Speaker>) {
    loop {
        let Some(held) = speaker.upgrade() else {
            return;
        };
        if held.session.ended() {
            return;
        }
        let notified = held.moved.notified();
        let wait = held.pump().unwrap_or(IDLE).min(IDLE);
        // Held at most one idle wait past its last owner: the next pass lets go.
        tokio::select! {
            () = notified => {}
            () = tokio::time::sleep(wait) => {}
        }
        drop(held);
    }
}

/// The wall clock, in milliseconds, as presence is stamped.
#[expect(
    clippy::cast_precision_loss,
    reason = "milliseconds since the epoch are exact in an f64"
)]
fn now_ms() -> f64 {
    tilo_clock::unix_now_ms() as f64
}
