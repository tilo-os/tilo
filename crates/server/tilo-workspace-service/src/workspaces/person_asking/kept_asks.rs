//! Pending asks persisted per workspace in the workspace store while a turn
//! awaits an answer.
//!
//! Nothing is written under a per-person folder (`people/<person>/`): each ask
//! is stored under its workspace's store directory (`workspaces/<ws>/asks/`),
//! readable only by the person asked and the asking agent. An answer is claimed
//! at most once across restarts: `started` is written before carrying out, and
//! a restart loads started asks as `interrupted` so nothing executes twice.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::remote::person_asking::{Asked, Words};
use tilo_lock::MutexRecover as _;

use super::{workspace_store_dir, workspace_store_root};
use crate::workspaces::Workspaces;

/// The most asks that wait for one person at a time.
pub const MAX_OPEN_ASKS: usize = 32;

/// What [`Workspaces::claim_ask`] refuses a second reader of one answer with.
pub const CARRIED_OUT: &str = "That answer was carried out already";
/// What it refuses an ask this server keeps nothing under with.
pub const NOTHING_WAITS: &str = "Nothing waits for an answer under that question";
/// What [`Workspaces::keep_ask`] refuses one more than [`MAX_OPEN_ASKS`] with.
pub const TOO_MANY_WAIT: &str = "Too many questions already wait for this person's answer";

/// An ask kept in the workspace store while an answer is pending.
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

impl KeptAsk {
    /// Whether `person` (root key, hex) or `agent` in `workspace` may read this ask.
    #[must_use]
    pub fn visible_to(&self, workspace: &str, person: &str, agent: Option<&str>) -> bool {
        self.workspace == workspace
            && self.person == person
            && agent.is_none_or(|id| self.agent == id)
    }
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

/// Pending asks loaded once from the workspace store.
#[derive(Default)]
pub(super) struct KeptAsks(Mutex<Option<Vec<KeptAsk>>>);

fn is_ask_id(ask: &str) -> bool {
    ask.len() == 32
        && ask
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn asks_dir(base: &Path, workspace: &str) -> PathBuf {
    workspace_store_dir(base, workspace).join("asks")
}

fn ask_path(base: &Path, workspace: &str, ask: &str) -> PathBuf {
    asks_dir(base, workspace).join(format!("{ask}.json"))
}

fn write_ask(base: &Path, ask: &KeptAsk) -> Result<(), String> {
    let bytes = serde_json::to_vec(ask).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &ask_path(base, &ask.workspace, &ask.ask),
        &bytes,
        0o600,
    )
    .map_err(|error| error.to_string())
}

fn read_ask(path: &Path) -> Option<KeptAsk> {
    let read = std::fs::read(path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()));
    match read {
        Ok(ask) => Some(ask),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "a kept ask does not read");
            None
        }
    }
}

fn kept_under(workspaces_root: &Path) -> Vec<KeptAsk> {
    let mut asks: Vec<KeptAsk> = std::fs::read_dir(workspaces_root)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|ws| {
            std::fs::read_dir(ws.path().join("asks"))
                .into_iter()
                .flatten()
        })
        .flatten()
        .filter_map(|file| read_ask(&file.path()))
        .filter(|ask: &KeptAsk| is_ask_id(&ask.ask))
        .map(|mut ask| {
            ask.interrupted = ask.started;
            ask
        })
        .collect();
    asks.sort_by(|a, b| {
        a.asked_at_ms
            .cmp(&b.asked_at_ms)
            .then_with(|| a.ask.cmp(&b.ask))
    });
    asks
}

impl Workspaces {
    fn with_asks<T>(&self, work: impl FnOnce(&mut Vec<KeptAsk>) -> T) -> T {
        let kept = self.asking();
        let mut asks = kept.asks.0.lock_recover();
        let open = asks.get_or_insert_with(|| kept_under(&workspace_store_root(&self.inner.dir)));
        let done = work(open);
        drop(asks);
        done
    }

    /// Keep `new` in its workspace store while `person` is asked about it.
    ///
    /// # Errors
    /// Too many asks wait for that person already, or it could not be persisted.
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
            write_ask(&self.inner.dir, &kept)?;
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

    /// The asks kept for `workspace` visible to `person` (and optionally `agent`).
    #[must_use]
    pub fn open_asks_for(
        &self,
        workspace: &str,
        person: &[u8; 32],
        agent: Option<&str>,
    ) -> Vec<KeptAsk> {
        let person = hex::encode(person);
        self.with_asks(|open| {
            open.iter()
                .filter(|ask| ask.visible_to(workspace, &person, agent))
                .cloned()
                .collect()
        })
    }

    /// Claim the ask `ask` for the one reader of its answer that carries it out.
    /// Persists `started = true` before returning so a restart never carries it out twice.
    ///
    /// # Errors
    /// Nothing is kept under that ask ([`NOTHING_WAITS`]), another reader
    /// claimed it ([`CARRIED_OUT`]), or the claim could not be written.
    pub fn claim_ask(&self, ask: &str) -> Result<KeptAsk, String> {
        let claimed = self.with_asks(|open| {
            let kept = open
                .iter_mut()
                .find(|kept| kept.ask == ask)
                .ok_or_else(|| NOTHING_WAITS.to_owned())?;
            if std::mem::replace(&mut kept.started, true) {
                return Err(CARRIED_OUT.to_owned());
            }
            Ok(kept.clone())
        })?;
        if let Err(error) = write_ask(&self.inner.dir, &claimed) {
            self.forget_ask(&claimed);
            return Err(error);
        }
        Ok(claimed)
    }

    /// Give back the claim on `ask` ([`Self::claim_ask`]) before anything of its answer was
    /// carried out: the next look reads its question again and carries the answer out then, or
    /// forgets it where the question was taken back meanwhile.
    ///
    /// # Errors
    /// The ask could not be written: it stays claimed, and a restart says it was interrupted.
    pub fn release_ask(&self, ask: &KeptAsk) -> Result<(), String> {
        let released = self.with_asks(|open| {
            let kept = open.iter_mut().find(|kept| kept.ask == ask.ask)?;
            kept.started = false;
            Some(kept.clone())
        });
        match released {
            Some(released) => write_ask(&self.inner.dir, &released),
            None => Ok(()),
        }
    }

    /// Forget `ask`.
    pub fn forget_ask(&self, ask: &KeptAsk) {
        self.with_asks(|open| open.retain(|kept| kept.ask != ask.ask));
        if !is_ask_id(&ask.ask) {
            return;
        }
        match std::fs::remove_file(ask_path(&self.inner.dir, &ask.workspace, &ask.ask)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, "a kept ask was not removed: it does nothing any more");
            }
        }
    }
}
