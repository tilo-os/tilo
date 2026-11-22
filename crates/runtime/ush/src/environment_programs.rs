//! The programs a workspace's server starts in the workspace's machine and serves from afar
//! ([`tilo_actions::remote::environment_programs`]): the half of the member that takes the
//! workspace's work.
//!
//! The member starts the program in the machine as its server would beside it, paid for by
//! the account the server names, and holds it for the server: what the program sends is kept,
//! numbered, until the server says it has
//! taken it, and what the server sends is handed to the program in order, each frame once.
//! A frame crosses as hex of the serving protocol's own bytes ([`framed`], [`unframed`]).
//!
//! The server is shown what it has not taken a window at a time ([`SHOWN`]), so one update
//! stays small however much the program prints; telling what it took shows it the next.
//!
//! A program is ended at once when its server cancels it, and when its server has not asked
//! after it for [`LEFT_AFTER`]: a program nobody answers any more only keeps its machine
//! awake. A server waiting for the program's next change is asking after it the whole time.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tilo_actions::remote::ActionCallError;
use tilo_actions::remote::environment_programs::{Frames, ProgramEnded, ServeProgram, TellProgram};

pub use crate::served_runs::wire::{FromRun as FromProgram, ToRun as ToProgram};
pub use crate::served_runs::{Heard, Over, ServeRun as Start};
use crate::served_runs::{ServedRun, wire};
use crate::workspace_shell::{Payer, WorkspaceShells};

/// How long a program is kept when its server no longer asks after it.
pub const LEFT_AFTER: Duration = Duration::from_secs(90);

/// How often a program that says nothing is looked at.
const LOOK: Duration = Duration::from_millis(100);

/// The most frames taken from a program before its server is shown them.
const BATCH: usize = 64;

/// The most of what a program sent that one update shows its server, as hex: a window of
/// the frames it has not taken, and always at least one frame.
pub const SHOWN: usize = 256 * 1024;

/// A program served in a workspace's machine, as whoever holds it tells and hears it.
pub trait Served: Send + Sync {
    /// Send `frame` to the program; `false` once it is over.
    fn tell(&self, frame: &ToProgram) -> bool;
    /// What the program says next, waited for at most `wait`.
    fn hear(&self, wait: Duration) -> Option<Heard>;
    /// End the program now, without asking it.
    fn cut(&self);
}

impl Served for ServedRun {
    fn tell(&self, frame: &ToProgram) -> bool {
        Self::tell(self, frame)
    }

    fn hear(&self, wait: Duration) -> Option<Heard> {
        Self::hear(self, wait)
    }

    fn cut(&self) {
        Self::cut(self);
    }
}

/// What starts a program in a workspace's machine: the workspace, who pays, and the program.
pub type Starts = dyn Fn(&str, &Payer, Start) -> io::Result<Box<dyn Served>> + Send + Sync;

/// One frame in the serving protocol's own bytes, as hex.
#[must_use]
pub fn framed<T: serde::Serialize>(frame: &T) -> Option<String> {
    let mut bytes = Vec::new();
    wire::send(&mut bytes, frame).ok()?;
    Some(hex::encode(bytes))
}

/// The frame `text` holds, when it holds one.
#[must_use]
pub fn unframed<T: serde::de::DeserializeOwned>(text: &str) -> Option<T> {
    let bytes = hex::decode(text).ok()?;
    wire::recv(&mut bytes.as_slice()).ok().flatten()
}

/// What a program sent that its server has not taken yet.
struct Kept {
    /// The number of the first frame kept, counted from the program's first.
    first: u64,
    frames: VecDeque<String>,
}

/// One program this member holds for a workspace's server.
pub struct Held {
    workspace: String,
    served: Box<dyn Served>,
    kept: Mutex<Kept>,
    /// How many of its server's frames were handed to it.
    handed: Mutex<u64>,
    /// When its server last asked after it.
    asked: Mutex<Instant>,
    /// How many of its server's waits for its next change are open.
    waiting: AtomicUsize,
    left_after: Duration,
    /// Its server took frames, so what it is shown changed without the program saying more.
    moved: AtomicBool,
}

impl Held {
    /// Start what `asked` names in `workspace`'s machine: by `starts` when the member was
    /// given one, else in the machine `shells` names, paid for by the account asked for.
    ///
    /// # Errors
    ///
    /// This member has no machines for its workspaces, or the program names a folder outside
    /// the workspace's files.
    pub(crate) fn start(
        shells: &WorkspaceShells,
        starts: Option<&Starts>,
        workspace: &str,
        asked: ServeProgram,
        left_after: Duration,
    ) -> Result<Self, String> {
        let payer = Payer {
            account: asked.account,
            size: linux_vm::hosted::size::VmSize::parse(&asked.size).unwrap_or_default(),
        };
        let start = Start {
            argv: asked.argv,
            env: asked.env,
            directory: PathBuf::from(asked.directory),
            endpoints: asked.endpoints,
            stop_grace: Duration::from_millis(asked.stop_grace_ms),
            agent_program: asked.agent_program,
            secrets: asked.secrets,
        };
        let served = match starts {
            Some(starts) => starts(workspace, &payer, start),
            None => shells
                .serve_run(workspace, Some(&payer), start)
                .map(|served| Box::new(served) as Box<dyn Served>),
        }
        .map_err(|error| error.to_string())?;
        Ok(Self {
            workspace: workspace.to_owned(),
            served,
            kept: Mutex::new(Kept {
                first: 0,
                frames: VecDeque::new(),
            }),
            handed: Mutex::new(0),
            asked: Mutex::new(Instant::now()),
            waiting: AtomicUsize::new(0),
            left_after,
            moved: AtomicBool::new(false),
        })
    }

    /// Whether this is a program of `workspace`.
    pub(crate) fn of(&self, workspace: &str) -> bool {
        self.workspace == workspace
    }

    /// Its server asked after it just now.
    pub(crate) fn asked_after(&self) {
        *self.asked.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    /// Its server waits for its next change until the answer is dropped: asking after it all
    /// that time, however long the wait.
    pub(crate) fn waited_on(self: &Arc<Self>) -> Waiting {
        self.asked_after();
        self.waiting.fetch_add(1, Ordering::SeqCst);
        Waiting(Arc::clone(self))
    }

    /// Its server has not asked after it for as long as it is kept.
    fn left(&self) -> bool {
        if self.waiting.load(Ordering::SeqCst) > 0 {
            return false;
        }
        let asked = *self.asked.lock().unwrap_or_else(PoisonError::into_inner);
        asked.elapsed() >= self.left_after
    }

    fn keep(&self, frame: &FromProgram) {
        if let Some(text) = framed(frame) {
            self.kept
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .frames
                .push_back(text);
        }
    }

    /// The first of what it sent that its server has not taken yet: a window of [`SHOWN`],
    /// or all of it when `whole`.
    fn frames(&self, whole: bool) -> Frames {
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let mut shown = 0_usize;
        let mut frames = Vec::new();
        for frame in &kept.frames {
            if !whole && !frames.is_empty() && shown.saturating_add(frame.len()) > SHOWN {
                break;
            }
            shown = shown.saturating_add(frame.len());
            frames.push(frame.clone());
        }
        let first = kept.first;
        drop(kept);
        Frames { first, frames }
    }

    /// Let go of the frames numbered below `taken`: its server has them.
    fn taken(&self, taken: u64) {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let mut gone = false;
        while kept.first < taken && kept.frames.pop_front().is_some() {
            kept.first = kept.first.saturating_add(1);
            gone = true;
        }
        let more = gone && !kept.frames.is_empty();
        drop(kept);
        if more {
            self.moved.store(true, Ordering::SeqCst);
        }
    }

    /// Hand it the frames of `told` it was not handed yet, in order, and let go of what its
    /// server has taken.
    ///
    /// # Errors
    ///
    /// A frame is none of the serving protocol's, or frames before these never came.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the count stays locked while its frames are handed on, so no two tellings mix"
    )]
    pub(crate) fn tell(&self, told: &TellProgram) -> Result<(), ActionCallError> {
        self.asked_after();
        self.taken(told.taken);
        let mut handed = self.handed.lock().unwrap_or_else(PoisonError::into_inner);
        let mut number = told.first;
        for text in &told.frames {
            if number > *handed {
                return Err(ActionCallError::InvalidRequest {
                    reason: "the frames before these never came".into(),
                });
            }
            if number == *handed {
                let frame: ToProgram =
                    unframed(text).ok_or_else(|| ActionCallError::InvalidRequest {
                        reason: "not a frame for a served program".into(),
                    })?;
                self.served.tell(&frame);
                *handed = handed.saturating_add(1);
            }
            number = number.saturating_add(1);
        }
        Ok(())
    }

    /// Follow it until it is over, saying each time what it sent that its server has not
    /// taken, and answer how it ended. It is ended at once when `stop` says so, and when its
    /// server no longer asks after it.
    pub(crate) fn follow(&self, stop: &AtomicBool, says: &dyn Fn(Frames)) -> ProgramEnded {
        let mut ending = false;
        loop {
            if !ending && (stop.load(Ordering::SeqCst) || self.left()) {
                self.served.cut();
                ending = true;
            }
            match self.served.hear(LOOK) {
                None => {
                    if self.moved.swap(false, Ordering::SeqCst) {
                        says(self.frames(false));
                    }
                }
                Some(Heard::Over(over)) => return self.ended(over),
                Some(Heard::Frame(frame)) => {
                    self.keep(&frame);
                    if let Some(over) = self.more() {
                        return self.ended(over);
                    }
                    self.moved.store(false, Ordering::SeqCst);
                    says(self.frames(false));
                }
            }
        }
    }

    /// Keep what else it has said by now, up to a batch; how it ended, when it did.
    fn more(&self) -> Option<Over> {
        for _ in 1..BATCH {
            match self.served.hear(Duration::ZERO) {
                Some(Heard::Frame(frame)) => self.keep(&frame),
                Some(Heard::Over(over)) => return Some(over),
                None => break,
            }
        }
        None
    }

    fn ended(&self, over: Over) -> ProgramEnded {
        ProgramEnded {
            last: self.frames(true),
            exit: over.exit,
            refusal: over.refusal,
            words: over.words,
        }
    }
}

/// One wait of a server for its program's next change ([`Held::waited_on`]).
pub(crate) struct Waiting(Arc<Held>);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.asked_after();
        self.0.waiting.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "environment_programs_tests.rs"]
mod tests;
