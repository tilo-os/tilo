//! The turn of a person's own agent that runs now, as a live activity follows it.
//!
//! An agent works for a person in a turn they started: it reads what they asked,
//! works through its calls, and answers, or asks them something. While every
//! device of theirs is elsewhere, the one thing that shows this is a live
//! activity on the lock screen. Its state is kept here, on the host that runs
//! their turn: not in the log of what they were told, which is what reaches them
//! when a turn is not running.
//!
//! It is small and it does not last: a turn that ended is kept only long enough
//! for the devices that show it to be told, and the newest are kept.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;

/// The most turns kept for one person: a newer turn is what the person waits on.
const KEPT: usize = 8;
/// The longest id of a turn, the workspace that holds it and its conversation, in bytes.
const MAX_ID_BYTES: usize = 200;
/// The longest name of an agent, the words asked and the words said, in bytes.
const MAX_WORDS_BYTES: usize = 600;
const MAX_NAME_BYTES: usize = 120;

/// What this host keeps of its people's running turns: one writer at a time, and
/// the people whose turns changed since whoever shows them last asked.
pub(super) struct Live {
    writing: Mutex<()>,
    moved: tokio::sync::watch::Sender<u64>,
    changed: Mutex<HashSet<[u8; 32]>>,
}

impl Live {
    pub(super) fn new() -> Self {
        Self {
            writing: Mutex::default(),
            moved: tokio::sync::watch::channel(0).0,
            changed: Mutex::default(),
        }
    }
}

/// `text`, cut at a character to at most `most` bytes where it is longer, and
/// then ending in an ellipsis.
fn clipped(text: &str, most: usize) -> String {
    if text.len() <= most {
        return text.to_owned();
    }
    let mut end = most.saturating_sub('…'.len_utf8());
    while end > 0 && !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", text.get(..end).unwrap_or_default().trim_end())
}

/// Where a turn of a person's own stands.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum LivePhase {
    /// It works.
    #[default]
    Working,
    /// It asked the person something and waits for them.
    Asking,
    /// It answered.
    Answered,
    /// A bound of it ended it.
    Stopped,
    /// It could not go on.
    Failed,
    /// Its person stopped it.
    Cancelled,
}

impl LivePhase {
    /// Whether the turn is over: nothing follows this.
    #[must_use]
    pub const fn is_over(self) -> bool {
        !matches!(self, Self::Working | Self::Asking)
    }
}

/// One turn of a person's own agent, as a device shows it while it runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveTurn {
    /// Its own name, which stays while it lasts.
    pub id: String,
    /// The workspace it runs in.
    pub workspace: String,
    /// The conversation it answers in, as `app/record`.
    pub conversation: String,
    /// The agent working, by the name its person knows it by.
    pub agent: String,
    /// What the person asked, in their words.
    pub asked: String,
    /// Where it stands.
    pub phase: LivePhase,
    /// What it answered, or why it stopped, where it is over.
    pub said: String,
    /// The call it is at among its answer's calls, and how many its answer has.
    pub step: u32,
    pub steps: u32,
    /// Whether words of it may leave the host: sealed for the device's own key
    /// when they do, and never in the open.
    pub outside: bool,
    /// When it last changed, in milliseconds since the epoch.
    pub at_ms: u64,
}

/// What is kept of a person's own turns.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Kept {
    #[serde(default)]
    live: VecDeque<LiveTurn>,
}

fn live_at(root: &Path) -> PathBuf {
    root.join("live").join("turn.json")
}

fn read_live(root: &Path) -> Kept {
    match std::fs::read(live_at(root)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            tracing::warn!(%error, "telling: a person's running turns do not read and start over");
            Kept::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Kept::default(),
        Err(error) => {
            tracing::warn!(%error, "telling: a person's running turns could not be read");
            Kept::default()
        }
    }
}

fn write_live(root: &Path, kept: &Kept) -> Result<(), String> {
    let bytes = serde_json::to_vec(kept).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(&live_at(root), &bytes, 0o600)
        .map_err(|error| error.to_string())
}

impl crate::PersonServices {
    /// Keep `turn` as the newest of `person`'s running turns, under its id.
    fn keep_live(&self, person: &[u8; 32], mut turn: LiveTurn) -> Result<LiveTurn, String> {
        turn.id = clipped(turn.id.trim(), MAX_ID_BYTES);
        turn.workspace = clipped(turn.workspace.trim(), MAX_ID_BYTES);
        turn.conversation = clipped(turn.conversation.trim(), MAX_ID_BYTES);
        turn.agent = clipped(turn.agent.trim(), MAX_NAME_BYTES);
        turn.asked = clipped(turn.asked.trim(), MAX_WORDS_BYTES);
        turn.said = clipped(turn.said.trim(), MAX_WORDS_BYTES);
        if turn.id.is_empty() {
            return Err("A turn kept for its person has an id".to_owned());
        }
        turn.at_ms = tilo_clock::unix_now_ms();
        let root = self.person_settings_root(person);
        {
            let _writing = self.inner.live.writing.lock_recover();
            let mut kept = read_live(&root);
            kept.live.retain(|known| known.id != turn.id);
            kept.live.push_back(turn.clone());
            while kept.live.len() > KEPT {
                kept.live.pop_front();
            }
            write_live(&root, &kept)?;
        }
        self.live_moved(person);
        Ok(turn)
    }

    /// `turn` started for `person`.
    ///
    /// # Errors
    /// It names nothing, or it could not be kept.
    pub fn live_started(&self, person: &[u8; 32], turn: &LiveTurn) -> Result<LiveTurn, String> {
        self.keep_live(person, turn.clone())
    }

    /// `turn` of `person`'s goes on: it is at `step` of `steps` calls and is
    /// `phase`.
    ///
    /// # Errors
    /// Nothing runs under that id, or it could not be kept.
    pub fn live_stepped(
        &self,
        person: &[u8; 32],
        id: &str,
        phase: LivePhase,
        (step, steps): (u32, u32),
    ) -> Result<LiveTurn, String> {
        let mut turn = self
            .live_turn(person, id)
            .ok_or_else(|| "Nothing ran under that id".to_owned())?;
        turn.phase = phase;
        turn.step = step;
        turn.steps = steps;
        self.keep_live(person, turn)
    }

    /// `turn` of `person`'s ended in `phase`, saying `said`.
    ///
    /// # Errors
    /// Nothing runs under that id, or it could not be kept.
    pub fn live_ended(
        &self,
        person: &[u8; 32],
        id: &str,
        phase: LivePhase,
        said: &str,
    ) -> Result<LiveTurn, String> {
        let mut turn = self
            .live_turn(person, id)
            .ok_or_else(|| "Nothing ran under that id".to_owned())?;
        turn.phase = phase;
        said.clone_into(&mut turn.said);
        self.keep_live(person, turn)
    }

    /// The turn `id` of `person`'s, where it is kept.
    #[must_use]
    pub fn live_turn(&self, person: &[u8; 32], id: &str) -> Option<LiveTurn> {
        let root = self.person_settings_root(person);
        let _writing = self.inner.live.writing.lock_recover();
        read_live(&root)
            .live
            .into_iter()
            .find(|known| known.id == id)
    }

    /// The turns of `person`'s kept now, the newest last.
    #[must_use]
    pub fn live_turns(&self, person: &[u8; 32]) -> Vec<LiveTurn> {
        let root = self.person_settings_root(person);
        let _writing = self.inner.live.writing.lock_recover();
        read_live(&root).live.into_iter().collect()
    }

    /// `person`'s running turns changed in a way a device shows.
    fn live_moved(&self, person: &[u8; 32]) {
        self.inner.live.changed.lock_recover().insert(*person);
        self.inner
            .live
            .moved
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Moves whenever anybody's running turns changed: what a sender wakes on.
    #[must_use]
    pub fn live_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.live.moved.subscribe()
    }

    /// The people whose running turns changed since this was last asked.
    #[must_use]
    pub fn live_changed_people(&self) -> Vec<[u8; 32]> {
        std::mem::take(&mut *self.inner.live.changed.lock_recover())
            .into_iter()
            .collect()
    }
}
