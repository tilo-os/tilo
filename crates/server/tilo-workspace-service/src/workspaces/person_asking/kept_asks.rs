//! Pending asks persisted per workspace in the workspace store while a turn
//! awaits an answer.
//!
//! All four kept-ask kinds (action/server asks in `asks/`, vault-use asks in
//! `vault-asks/`, calendar-connect asks in `calendar-asks/`, and mail-connect
//! asks in `mail-asks/`) share [`KeptAskStore`], keyed by [`KeptAskKind`].
//!
//! Nothing is written under a per-person folder (`people/<person>/`): each ask
//! is stored under its workspace's store directory (`workspaces/<ws>/<dir>/`),
//! readable only by this service. An answer is claimed at most once across
//! restarts: `started` is written before carrying out, and a restart loads
//! started asks as `interrupted` so nothing executes twice.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::remote::person_asking::{Asked, Words};
use tilo_lock::MutexRecover as _;

use super::{workspace_store_dir, workspace_store_root};
use crate::workspaces::Workspaces;

/// The most asks that wait for one person at a time.
pub const MAX_OPEN_ASKS: usize = 32;

/// What claiming an ask refuses a second reader of one answer with.
pub const CARRIED_OUT: &str = "That answer was carried out already";
/// What claiming an ask refuses an ask this server keeps nothing under with.
pub const NOTHING_WAITS: &str = "Nothing waits for an answer under that question";
/// What keeping an ask refuses one more than [`MAX_OPEN_ASKS`] with.
pub const TOO_MANY_WAIT: &str = "Too many questions already wait for this person's answer";

/// One kind of kept ask stored under `workspaces/<hex(workspace)>/<DIR>/<id>.json`.
///
/// Each kind supplies only what truly differs: its directory, its fields, its
/// per-person cap, and how refusal strings map into its callers' error type.
pub(crate) trait KeptAskKind: Clone + Serialize + DeserializeOwned {
    /// The error type callers of this kind return.
    type Error;
    /// Subdirectory under the workspace store directory.
    const DIR: &'static str;
    /// Warning message logged when a kept file on disk fails to read or parse.
    const READ_FAILED: &'static str;
    /// Per-person limit on concurrent waiting asks and the refusal message, or
    /// `None` where no limit applies.
    const MAX_OPEN: Option<(usize, &'static str)>;

    /// The ask's own 32-hex-character id.
    fn id(&self) -> &str;
    /// The workspace it was asked in.
    fn workspace(&self) -> &str;
    /// The person asked: their root key, hex.
    fn person(&self) -> &str;
    /// Whether an answer is being carried out (or was when the server stopped).
    fn started(&self) -> bool;
    /// Update whether an answer is being carried out.
    fn set_started(&mut self, started: bool);
    /// Whether this ask was read back started after a server restart.
    fn interrupted(&self) -> bool;
    /// Mark whether this ask was interrupted by a restart.
    fn set_interrupted(&mut self, interrupted: bool);
    /// Creation timestamp used to order open asks oldest first.
    fn asked_at(&self) -> u64;
    /// Map a refusal or persistence failure into this kind's error type.
    fn refused(reason: impl Into<String>) -> Self::Error;
}

/// Whether `ask` is a valid kept-ask id: 32 lowercase hexadecimal characters.
pub(crate) fn is_ask_id(ask: &str) -> bool {
    ask.len() == 32
        && ask
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Deterministic 32-hex-character ask id hashed from who asked, what they asked,
/// and when.
pub(crate) fn hashed_ask_id(person: &str, agent: &str, what: &str, at: u64) -> String {
    blake3::hash(format!("{person}\0{agent}\0{what}\0{at}").as_bytes())
        .to_hex()
        .chars()
        .take(32)
        .collect()
}

/// Pending asks of kind `K`, lazily loaded once from the workspace store.
pub(crate) struct KeptAskStore<K>(Mutex<Option<Vec<K>>>);

impl<K> Default for KeptAskStore<K> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl<K: KeptAskKind> KeptAskStore<K> {
    fn path(base: &Path, workspace: &str, ask: &str) -> PathBuf {
        workspace_store_dir(base, workspace)
            .join(K::DIR)
            .join(format!("{ask}.json"))
    }

    fn write(base: &Path, ask: &K) -> Result<(), String> {
        if !is_ask_id(ask.id()) {
            return Err("invalid kept ask id".to_owned());
        }
        let bytes = serde_json::to_vec(ask).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write_creating_parent_with_mode(
            &Self::path(base, ask.workspace(), ask.id()),
            &bytes,
            0o600,
        )
        .map_err(|error| error.to_string())
    }

    fn read(path: &Path) -> Option<K> {
        let read = std::fs::read(path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()));
        match read {
            Ok(ask) => Some(ask),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "{}", K::READ_FAILED);
                None
            }
        }
    }

    fn load_all(base: &Path) -> Vec<K> {
        let mut asks = Vec::new();
        for ws in std::fs::read_dir(workspace_store_root(base))
            .into_iter()
            .flatten()
            .flatten()
        {
            let ws_name = ws.file_name();
            for file in std::fs::read_dir(ws.path().join(K::DIR))
                .into_iter()
                .flatten()
                .flatten()
            {
                let path = file.path();
                let Some(mut ask) = Self::read(&path) else {
                    continue;
                };
                let expected_file = format!("{}.json", ask.id());
                let expected_ws = hex::encode(ask.workspace().as_bytes());
                if !is_ask_id(ask.id())
                    || file.file_name().to_str() != Some(expected_file.as_str())
                    || ws_name.to_str() != Some(expected_ws.as_str())
                {
                    tracing::warn!(
                        path = %path.display(),
                        ask = ask.id(),
                        "ignoring a kept ask whose id or location does not match its file"
                    );
                    continue;
                }
                ask.set_interrupted(ask.started());
                asks.push(ask);
            }
        }
        asks.sort_by(|a, b| {
            a.asked_at()
                .cmp(&b.asked_at())
                .then_with(|| a.id().cmp(b.id()))
        });
        asks
    }

    fn remove_file(base: &Path, workspace: &str, ask: &str) {
        if !is_ask_id(ask) {
            return;
        }
        match std::fs::remove_file(Self::path(base, workspace, ask)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    dir = K::DIR,
                    %error,
                    "a kept ask was not removed: it does nothing any more"
                );
            }
        }
    }

    fn with_open<T>(&self, base: &Path, work: impl FnOnce(&mut Vec<K>) -> T) -> T {
        let mut asks = self.0.lock_recover();
        let open = asks.get_or_insert_with(|| Self::load_all(base));
        let done = work(open);
        drop(asks);
        done
    }

    /// Whether any open ask currently matches `predicate`.
    #[cfg(feature = "agents")]
    pub(crate) fn any(&self, base: &Path, predicate: impl Fn(&K) -> bool) -> bool {
        self.with_open(base, |open| open.iter().any(predicate))
    }

    /// Every open ask kept for `workspace`, oldest first.
    pub(crate) fn open(&self, base: &Path, workspace: &str) -> Vec<K> {
        self.open_matching(base, |ask| ask.workspace() == workspace)
    }

    /// Every open ask matching `predicate`, oldest first.
    pub(crate) fn open_matching(&self, base: &Path, predicate: impl Fn(&K) -> bool) -> Vec<K> {
        self.with_open(base, |open| {
            open.iter().filter(|ask| predicate(ask)).cloned().collect()
        })
    }

    /// Keep a new ask if none matching `same` already waits, enforcing
    /// [`KeptAskKind::MAX_OPEN`] for `person` before building and persisting it.
    /// Returns `Ok(None)` when an ask matching `same` already waits.
    pub(crate) fn keep<I>(
        &self,
        base: &Path,
        person: &str,
        input: I,
        same: impl Fn(&K, &I) -> bool,
        build: impl FnOnce(I) -> Result<K, K::Error>,
    ) -> Result<Option<K>, K::Error> {
        self.with_open(base, |open| {
            if open.iter().any(|kept| same(kept, &input)) {
                return Ok(None);
            }
            if let Some((limit, too_many)) = K::MAX_OPEN
                && open.iter().filter(|kept| kept.person() == person).count() >= limit
            {
                return Err(K::refused(too_many));
            }
            let kept = build(input)?;
            Self::write(base, &kept).map_err(K::refused)?;
            open.push(kept.clone());
            Ok(Some(kept))
        })
    }

    /// Persist and record a replacement ask unconditionally (used when a
    /// refused password spawns a fresh retry ask after forgetting the spent one).
    #[cfg(feature = "agents")]
    pub(crate) fn insert(&self, base: &Path, ask: K) -> Result<K, String> {
        self.with_open(base, |open| {
            Self::write(base, &ask)?;
            open.push(ask.clone());
            Ok(ask)
        })
    }

    /// Claim `ask` for the one reader of its answer that carries it out.
    /// Persists `started = true` before returning so a restart never carries it
    /// out twice. If persisting the claim fails, the ask is forgotten so it is
    /// neither executed nor claimed again.
    pub(crate) fn claim(&self, base: &Path, ask: &str) -> Result<K, K::Error> {
        if !is_ask_id(ask) {
            return Err(K::refused(NOTHING_WAITS));
        }
        self.with_open(base, |open| {
            let Some(kept) = open.iter_mut().find(|kept| kept.id() == ask) else {
                return Err(K::refused(NOTHING_WAITS));
            };
            if kept.started() {
                return Err(K::refused(CARRIED_OUT));
            }
            kept.set_started(true);
            let claimed = kept.clone();
            if let Err(error) = Self::write(base, &claimed) {
                open.retain(|kept| kept.id() != ask);
                Self::remove_file(base, claimed.workspace(), claimed.id());
                return Err(K::refused(error));
            }
            Ok(claimed)
        })
    }

    /// Give back the claim on `ask` before anything of its answer was carried
    /// out. Clears `started` in memory before writing to disk so that if the
    /// write fails, this process can still claim and carry out the answer on
    /// the next look, while a restart still sees `started = true` on disk as
    /// interrupted.
    pub(crate) fn release(&self, base: &Path, ask: &K) -> Result<(), K::Error> {
        let released = self.with_open(base, |open| {
            let kept = open.iter_mut().find(|kept| kept.id() == ask.id())?;
            kept.set_started(false);
            Some(kept.clone())
        });
        match released {
            Some(released) => Self::write(base, &released).map_err(K::refused),
            None => Ok(()),
        }
    }

    /// Forget `ask` in `workspace`, removing it from memory and disk.
    pub(crate) fn forget(&self, base: &Path, workspace: &str, ask: &str) {
        self.with_open(base, |open| open.retain(|kept| kept.id() != ask));
        Self::remove_file(base, workspace, ask);
    }

    /// Forget `ask` only if it was loaded as interrupted after a restart.
    pub(crate) fn forget_if_interrupted(&self, base: &Path, ask: &K) {
        if ask.interrupted() {
            self.forget(base, ask.workspace(), ask.id());
        }
    }
}

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

impl KeptAskKind for KeptAsk {
    type Error = String;
    const DIR: &'static str = "asks";
    const READ_FAILED: &'static str = "a kept ask does not read";
    const MAX_OPEN: Option<(usize, &'static str)> = Some((MAX_OPEN_ASKS, TOO_MANY_WAIT));

    fn id(&self) -> &str {
        &self.ask
    }

    fn workspace(&self) -> &str {
        &self.workspace
    }

    fn person(&self) -> &str {
        &self.person
    }

    fn started(&self) -> bool {
        self.started
    }

    fn set_started(&mut self, started: bool) {
        self.started = started;
    }

    fn interrupted(&self) -> bool {
        self.interrupted
    }

    fn set_interrupted(&mut self, interrupted: bool) {
        self.interrupted = interrupted;
    }

    fn asked_at(&self) -> u64 {
        self.asked_at_ms
    }

    fn refused(reason: impl Into<String>) -> Self::Error {
        reason.into()
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

impl Workspaces {
    /// Keep `new` in its workspace store while `person` is asked about it.
    ///
    /// # Errors
    /// Too many asks wait for that person already, or it could not be persisted.
    pub fn keep_ask(&self, person: &[u8; 32], new: NewAsk) -> Result<Option<KeptAsk>, String> {
        let person = hex::encode(person);
        self.asking().asks.keep(
            &self.inner.dir,
            &person,
            new,
            |kept, new| {
                kept.person == person
                    && kept.workspace == new.workspace
                    && kept.agent == new.agent
                    && kept.carrier == new.carrier
                    && kept.kept == new.kept
            },
            |new| {
                Ok(KeptAsk {
                    ask: hex::encode(rand::random::<[u8; 16]>()),
                    workspace: new.workspace,
                    person: person.clone(),
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
                })
            },
        )
    }

    /// The asks kept for `workspace`, oldest first.
    #[must_use]
    pub fn open_asks(&self, workspace: &str) -> Vec<KeptAsk> {
        self.asking().asks.open(&self.inner.dir, workspace)
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
        self.asking().asks.open_matching(&self.inner.dir, |ask| {
            ask.visible_to(workspace, &person, agent)
        })
    }

    /// Claim the ask `ask` for the one reader of its answer that carries it out.
    /// Persists `started = true` before returning so a restart never carries it out twice.
    ///
    /// # Errors
    /// Nothing is kept under that ask ([`NOTHING_WAITS`]), another reader
    /// claimed it ([`CARRIED_OUT`]), or the claim could not be written.
    pub fn claim_ask(&self, ask: &str) -> Result<KeptAsk, String> {
        self.asking().asks.claim(&self.inner.dir, ask)
    }

    /// Give back the claim on `ask` ([`Self::claim_ask`]) before anything of its answer was
    /// carried out: the next look reads its question again and carries the answer out then, or
    /// forgets it where the question was taken back meanwhile.
    ///
    /// # Errors
    /// The ask could not be written: its claim is already cleared in memory so this process
    /// can claim it again on the next look, while on disk it stays started so a restart says
    /// it was interrupted.
    pub fn release_ask(&self, ask: &KeptAsk) -> Result<(), String> {
        self.asking().asks.release(&self.inner.dir, ask)
    }

    /// Forget `ask`.
    pub fn forget_ask(&self, ask: &KeptAsk) {
        self.asking()
            .asks
            .forget(&self.inner.dir, &ask.workspace, &ask.ask);
    }
}
