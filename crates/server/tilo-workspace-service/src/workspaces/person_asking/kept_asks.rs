//! In-memory pending asks scoped to a workspace while a turn awaits an answer.
//!
//! Pending asks are never written to per-person server disk folders: every
//! interactive approval routes through [`Workspaces::ask_person_confirmation`]
//! and lasting permissions live as `tilo-action-policy` grants on the
//! workspace.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::remote::person_asking::{Asked, Words};
use tilo_lock::MutexRecover as _;

use crate::workspaces::Workspaces;

/// The most asks that wait for one person at a time.
pub const MAX_OPEN_ASKS: usize = 32;

/// What [`Workspaces::claim_ask`] refuses a second reader of one answer with.
pub const CARRIED_OUT: &str = "That answer was carried out already";
/// What it refuses an ask this server keeps nothing under with.
pub const NOTHING_WAITS: &str = "Nothing waits for an answer under that question";
/// What [`Workspaces::keep_ask`] refuses one more than [`MAX_OPEN_ASKS`] with.
pub const TOO_MANY_WAIT: &str = "Too many questions already wait for this person's answer";

/// An ask kept in memory while an answer is pending.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeptAsk {
    /// The ask's own id.
    pub ask: String,
    /// The workspace it was asked in.
    pub workspace: String,
    /// The person asked: their root key, hex.
    pub person: String,
    /// The agent that asked, by its id.
    pub agent: String,
    /// The name its person knows it under.
    pub name: String,
    /// Who carries the answer out, by a word of the code that asked.
    pub carrier: String,
    /// What that carrier does on the answer: its own to read. It holds no secret.
    pub kept: Value,
    /// What an answer of always lets stand; `None` offers no such answer.
    #[serde(default)]
    pub always: Option<Asked>,
    /// The system's own words for what is asked.
    #[serde(default)]
    pub words: Words,
    /// Where whoever asked put the question.
    pub asked_in: Value,
    /// Whether an answer is being carried out.
    #[serde(default)]
    pub started: bool,
    /// Whether this server stopped while an answer was being carried out.
    #[serde(skip)]
    pub interrupted: bool,
    /// When it was asked, in milliseconds since the Unix epoch.
    pub asked_at_ms: u64,
}

/// An ask to keep.
#[derive(Clone, Debug)]
pub struct NewAsk {
    pub workspace: String,
    pub agent: String,
    pub name: String,
    pub carrier: String,
    pub kept: Value,
    pub always: Option<Asked>,
    pub words: Words,
    pub asked_in: Value,
}

/// In-memory pending asks, never persisted to per-person disk storage.
#[derive(Default)]
pub(super) struct KeptAsks(Mutex<Vec<KeptAsk>>);

impl Workspaces {
    fn with_asks<T>(&self, work: impl FnOnce(&mut Vec<KeptAsk>) -> T) -> T {
        let kept = self.asking();
        let mut asks = kept.asks.0.lock_recover();
        let done = work(&mut asks);
        drop(asks);
        done
    }

    /// Keep `new` in memory while `person` is asked about it.
    ///
    /// # Errors
    /// Too many asks wait for that person already.
    pub fn keep_ask(&self, person: &[u8; 32], new: NewAsk) -> Result<Option<KeptAsk>, String> {
        let person = hex::encode(person);
        self.with_asks(|open| {
            let waits = open.iter().any(|kept| {
                kept.person == person
                    && kept.workspace == new.workspace
                    && kept.agent == new.agent
                    && kept.carrier == new.carrier
                    && kept.kept == new.kept
            });
            if waits {
                return Ok(None);
            }
            if open.iter().filter(|kept| kept.person == person).count() >= MAX_OPEN_ASKS {
                return Err(TOO_MANY_WAIT.to_owned());
            }
            let kept = KeptAsk {
                ask: hex::encode(rand::random::<[u8; 16]>()),
                workspace: new.workspace,
                person,
                agent: new.agent,
                name: new.name,
                carrier: new.carrier,
                kept: new.kept,
                always: new.always,
                words: new.words,
                asked_in: new.asked_in,
                started: false,
                interrupted: false,
                asked_at_ms: tilo_clock::unix_now_ms(),
            };
            open.push(kept.clone());
            Ok(Some(kept))
        })
    }

    /// The asks kept for `workspace`, oldest first.
    #[must_use]
    pub fn open_asks(&self, workspace: &str) -> Vec<KeptAsk> {
        self.with_asks(|open| {
            open.iter()
                .filter(|ask| ask.workspace == workspace)
                .cloned()
                .collect()
        })
    }

    /// Claim the ask `ask` for the one reader of its answer that carries it out.
    ///
    /// # Errors
    /// Nothing is kept under that ask ([`NOTHING_WAITS`]), or another reader
    /// claimed it ([`CARRIED_OUT`]).
    pub fn claim_ask(&self, ask: &str) -> Result<KeptAsk, String> {
        self.with_asks(|open| {
            let kept = open
                .iter_mut()
                .find(|kept| kept.ask == ask)
                .ok_or_else(|| NOTHING_WAITS.to_owned())?;
            if std::mem::replace(&mut kept.started, true) {
                return Err(CARRIED_OUT.to_owned());
            }
            Ok(kept.clone())
        })
    }

    /// Forget `ask`.
    pub fn forget_ask(&self, ask: &KeptAsk) {
        self.with_asks(|open| open.retain(|kept| kept.ask != ask.ask));
    }
}
