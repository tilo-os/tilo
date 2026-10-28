//! The ledger: what just happened, append-only, in one place.
//!
//! A ledger entry is an outcome the system does not need a person for
//! ("Backup finished", "3 files moved to Trash", "Pushed to origin/main").
//! It has an owner (who did it), what it says, when, and — when the thing can
//! be undone — what to undo; Activities's History shows the entries beside
//! the workspace's changes, searchable and attributable like them.
//!
//! Three things an entry is not:
//!
//! * **it does not expire.** There is no lifetime, no dismissal and no
//!   "clears on read": the record is the record. (The registry keeps a bound
//!   on how many entries are held in memory — [`MAX_ENTRIES`] — because a
//!   process that ran for a month must not grow without end; the bound is a
//!   memory limit, not a lifetime, and it holds thousands of outcomes.)
//! * **it does not steal focus.** Nothing here opens a window, presses a
//!   button or takes the keyboard.
//! * **it is not a notice.** A thing that needs a decision is not an entry:
//!   it is [`crate::attention`]'s.

use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::attention::Owner;
use crate::attention::Target;
use crate::lock::MutexRecover as _;
use crate::time::unix_now_ms;

/// How many entries are held in memory. A bound on memory, not a lifetime:
/// thousands of outcomes is months of use, and the record is read, not
/// pruned.
pub const MAX_ENTRIES: usize = 4096;

/// One thing that happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    id: u64,
    owner: Owner,
    what: String,
    about: Option<String>,
    because: Option<String>,
    at_ms: u64,
    target: Option<Target>,
    undo: Option<String>,
}

impl Entry {
    /// An entry `owner` records, saying `what`.
    #[must_use]
    pub fn new(owner: Owner, what: impl Into<String>) -> Self {
        Self {
            id: 0,
            owner,
            what: what.into(),
            about: None,
            because: None,
            at_ms: unix_now_ms(),
            target: None,
            undo: None,
        }
    }

    /// What it is about ("/home/you/Notes", "Budget").
    #[must_use]
    pub fn about(mut self, about: impl Into<String>) -> Self {
        self.about = Some(about.into());
        self
    }

    /// Why, in one clause.
    #[must_use]
    pub fn because(mut self, because: impl Into<String>) -> Self {
        self.because = Some(because.into());
        self
    }

    /// What it is about, for the app.
    #[must_use]
    pub fn subject(mut self, target: Target) -> Self {
        self.target = Some(target);
        self
    }

    /// The verb that undoes it, when it can be undone
    /// ([`crate::attention::Action::verb`]).
    #[must_use]
    pub fn undoable(mut self, verb: impl Into<String>) -> Self {
        self.undo = Some(verb.into());
        self
    }

    /// Its identity, in the order entries were recorded.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Who did it.
    #[must_use]
    pub const fn owner(&self) -> &Owner {
        &self.owner
    }

    /// What happened.
    #[must_use]
    pub fn what(&self) -> &str {
        &self.what
    }

    /// What it is about.
    #[must_use]
    pub fn about_line(&self) -> Option<&str> {
        self.about.as_deref()
    }

    /// Why.
    #[must_use]
    pub fn because_line(&self) -> Option<&str> {
        self.because.as_deref()
    }

    /// When it happened, in milliseconds since the epoch.
    #[must_use]
    pub const fn at_ms(&self) -> u64 {
        self.at_ms
    }

    /// What it is about, for the app.
    #[must_use]
    pub const fn target(&self) -> Option<Target> {
        self.target
    }

    /// Whether it can be undone, and by which verb.
    #[must_use]
    pub fn undo_verb(&self) -> Option<&str> {
        self.undo.as_deref()
    }

    /// The line a History row shows under the outcome: what it is about and
    /// why, as one clause.
    #[must_use]
    pub fn detail_line(&self) -> String {
        match (&self.about, &self.because) {
            (None, None) => String::new(),
            (Some(about), None) => about.clone(),
            (None, Some(because)) => because.clone(),
            (Some(about), Some(because)) => crate::i18n::widget_format(
                "attention-because",
                &[
                    ("about", crate::i18n::ArgValue::from(about.clone())),
                    ("because", crate::i18n::ArgValue::from(because.clone())),
                ],
            ),
        }
    }

    /// Whether it matches `query` in what it says, what it is about or who
    /// did it: the history's search, run on the record.
    #[must_use]
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let fields = [
            self.what.as_str(),
            self.about.as_deref().unwrap_or_default(),
            self.because.as_deref().unwrap_or_default(),
            self.owner.name.as_str(),
            self.owner.app.as_str(),
        ];
        fields
            .iter()
            .any(|field| field.to_lowercase().contains(&query))
    }
}

/// Append `entry` to the ledger; its identity.
pub fn record(entry: Entry) -> u64 {
    let mut registry = registry();
    let id = registry.next;
    registry.next = registry.next.wrapping_add(1);
    let mut entry = entry;
    entry.id = id;
    registry.entries.push(entry);
    if registry.entries.len() > MAX_ENTRIES {
        let over = registry.entries.len().saturating_sub(MAX_ENTRIES);
        registry.entries.drain(..over);
    }
    id
}

/// Every entry, oldest first.
#[must_use]
pub fn entries() -> Vec<Entry> {
    registry().entries.clone()
}

/// The entries newest first, which is the order History lists them in.
#[must_use]
pub fn newest_first() -> Vec<Entry> {
    let mut entries = entries();
    entries.reverse();
    entries
}

/// How many entries the ledger holds.
#[must_use]
pub fn len() -> usize {
    registry().entries.len()
}

/// The entries whose outcome, subject or author match `query`, newest
/// first.
#[must_use]
pub fn search(query: &str) -> Vec<Entry> {
    let mut found: Vec<Entry> = registry()
        .entries
        .iter()
        .filter(|entry| entry.matches(query))
        .cloned()
        .collect();
    found.reverse();
    found
}

/// Take every entry away: a test's clean slate, and what a session reset
/// does.
#[doc(hidden)]
pub fn clear() {
    let mut registry = registry();
    registry.entries.clear();
}

#[derive(Default)]
struct Registry {
    next: u64,
    entries: Vec<Entry>,
}

fn registry() -> MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Mutex::new(Registry::default()))
        .lock_recover()
}
