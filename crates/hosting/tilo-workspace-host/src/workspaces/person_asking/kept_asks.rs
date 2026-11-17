//! What this server keeps of an agent's ask while its person is asked about
//! it in a conversation.
//!
//! An agent a person keeps on this server asks before it does what their
//! grants leave to them, and the question waits in the conversation for as
//! long as they take: an hour, a night, across a restart of this server. What
//! is done on their answer must be what was asked, so the server keeps it
//! ([`KeptAsk`]): under the person's own root, beside their vault, readable
//! by this server alone. The question in the document carries only the ask's
//! id. A question an agent wrote itself has nothing kept here and does
//! nothing, whatever it shows.
//!
//! An answer is carried out at most once. Whoever reads it claims the ask
//! first ([`Workspaces::claim_ask`]), which is written down before anything
//! is done: a second reader is refused, and a server that stopped in the middle
//! finds the ask started and does not start it again.
//!
//! Nothing here is reached from a transport. The code that works the
//! agents' turns keeps, claims and forgets.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::remote::person_asking::{Asked, Words};
use tilo_lock::MutexRecover as _;

use crate::workspaces::Workspaces;

/// The most asks that wait for one person at a time: an agent that goes on
/// asking is told so instead of filling their conversation.
pub const MAX_OPEN_ASKS: usize = 32;

/// What [`Workspaces::claim_ask`] refuses a second reader of one answer
/// with: the first is carrying it out, or did.
pub const CARRIED_OUT: &str = "That answer was carried out already";
/// What it refuses an ask this server keeps nothing under with.
pub const NOTHING_WAITS: &str = "Nothing waits for an answer under that question";
/// What [`Workspaces::keep_ask`] refuses one more than [`MAX_OPEN_ASKS`]
/// with.
pub const TOO_MANY_WAIT: &str = "Too many questions already wait for this person's answer";

/// An ask this server keeps while its person is asked about it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeptAsk {
    /// The ask's own id, which the question carries.
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
    /// What that carrier does on the answer: its own to read. It holds no
    /// secret.
    pub kept: Value,
    /// What an answer of always lets stand; `None` offers no such answer.
    #[serde(default)]
    pub always: Option<Asked>,
    /// The system's own words for what is asked.
    #[serde(default)]
    pub words: Words,
    /// Where whoever asked put the question: theirs to read, kept here so
    /// it is found again after a restart.
    pub asked_in: Value,
    /// Whether an answer is being carried out: said before it starts.
    #[serde(default)]
    pub started: bool,
    /// Whether this server stopped while an answer was being carried out: the
    /// ask was read back started from its disk, and nobody carries it out
    /// now.
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

/// The asks this server keeps, read from its disk once.
#[derive(Default)]
pub(super) struct KeptAsks(Mutex<Option<Vec<KeptAsk>>>);

/// Whether `ask` is an ask's id: what names a file here.
fn is_ask_id(ask: &str) -> bool {
    ask.len() == 32
        && ask
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn asks_dir(root: &Path) -> PathBuf {
    root.join("asking").join("asks")
}

fn ask_path(root: &Path, ask: &str) -> PathBuf {
    asks_dir(root).join(format!("{ask}.json"))
}

/// Keep `ask` under its person's `root`, readable by this server alone.
fn write(root: &Path, ask: &KeptAsk) -> Result<(), String> {
    let bytes = serde_json::to_vec(ask).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &ask_path(root, &ask.ask),
        &bytes,
        0o600,
    )
    .map_err(|error| error.to_string())
}

/// The ask kept at `path`. One that does not read is said and left out: it
/// does nothing.
fn read(path: &Path) -> Option<KeptAsk> {
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

/// Every ask kept under the people's roots in `people`, oldest first. One
/// that was started is read back as interrupted: this server stopped while
/// its answer was being carried out.
fn kept_under(people: &Path) -> Vec<KeptAsk> {
    let mut asks: Vec<KeptAsk> = std::fs::read_dir(people)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|person| {
            std::fs::read_dir(asks_dir(&person.path()))
                .into_iter()
                .flatten()
        })
        .flatten()
        .filter_map(|file| read(&file.path()))
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
    /// Work on the asks this server keeps, read from its disk the first time.
    fn with_asks<T>(&self, work: impl FnOnce(&mut Vec<KeptAsk>) -> T) -> T {
        let kept = self.asking();
        let mut asks = kept.asks.0.lock_recover();
        let open = asks.get_or_insert_with(|| kept_under(&self.inner.dir.join("people")));
        let done = work(open);
        drop(asks);
        done
    }

    /// The root `person` (a root key, hex) names, when it names one.
    fn asked_root(&self, person: &str) -> Option<PathBuf> {
        let mut root = [0; 32];
        hex::decode_to_slice(person, &mut root).ok()?;
        Some(self.person_settings_root(&root))
    }

    /// Keep `new` while `person` is asked about it. `None` when the same
    /// agent asked the same before and they have not answered yet: the one
    /// question stands.
    ///
    /// # Errors
    /// Too many asks wait for that person already, or it could not be
    /// written down.
    pub fn keep_ask(&self, person: &[u8; 32], new: NewAsk) -> Result<Option<KeptAsk>, String> {
        let root = self.person_settings_root(person);
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
            write(&root, &kept)?;
            open.push(kept.clone());
            Ok(Some(kept))
        })
    }

    /// The asks kept for `workspace`, oldest first: what whoever works its
    /// agents reads answers for. Cheap where none waits.
    #[must_use]
    pub fn open_asks(&self, workspace: &str) -> Vec<KeptAsk> {
        self.with_asks(|open| {
            open.iter()
                .filter(|ask| ask.workspace == workspace)
                .cloned()
                .collect()
        })
    }

    /// Claim the ask `ask` for the one reader of its answer that carries it
    /// out. It is said in this server's memory and on its disk before
    /// anything else is said or done, so two readers of one answer carry it
    /// out once, and a server that stops finds it started.
    ///
    /// # Errors
    /// Nothing is kept under that ask ([`NOTHING_WAITS`]), another reader
    /// claimed it ([`CARRIED_OUT`]), or the claim could not be written down.
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
        let said = self
            .asked_root(&claimed.person)
            .ok_or_else(|| NOTHING_WAITS.to_owned())
            .and_then(|root| write(&root, &claimed));
        if let Err(error) = said {
            self.forget_ask(&claimed);
            return Err(error);
        }
        Ok(claimed)
    }

    /// Forget `ask`: it does nothing any more.
    pub fn forget_ask(&self, ask: &KeptAsk) {
        self.with_asks(|open| open.retain(|kept| kept.ask != ask.ask));
        if !is_ask_id(&ask.ask) {
            return;
        }
        let Some(root) = self.asked_root(&ask.person) else {
            return;
        };
        match std::fs::remove_file(ask_path(&root, &ask.ask)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, "a kept ask was not removed: it does nothing any more");
            }
        }
    }
}
