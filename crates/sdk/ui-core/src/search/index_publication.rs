//! An app's search entries as the system's persistent search index takes
//! them.
//!
//! A running app publishes what its search finds (each entry's title,
//! secondary line and short fields, keyed by the stable resource id a hit
//! opens) to the session over its certified broker route, and the session
//! keeps it after the app closes, so the app's content stays findable
//! whether or not it runs. Only short metadata crosses: prose bodies stay in
//! the app, and a secret-bearing entry ([`IndexEntry::secret`]) carries what
//! the app chose to show of it (a name, a username, a site), never a secret.
//!
//! The publication is incremental: a [`Publisher`] remembers what its
//! connection last sent and sends an [`IndexDelta`] of what changed, the
//! whole set again (`reset`) only on a new route.

use std::collections::{BTreeMap, HashMap};

/// Encoded bytes one [`IndexDelta`] may take; a larger change is sent as
/// several.
pub const MAX_DELTA_BYTES: usize = 512 * 1024;
/// Bytes of one text an entry carries (its title, its secondary line, one
/// field); longer text is cut at a character boundary.
pub const MAX_TEXT_BYTES: usize = 512;
/// Bytes of a resource id, action or key.
pub const MAX_ID_BYTES: usize = 1024;
/// Short fields one entry is found by.
pub const MAX_FIELDS: usize = 32;
/// Entries one publication holds; the rest of a larger set is not indexed.
pub const MAX_ENTRIES: usize = 20_000;

/// How much a match in one of an entry's fields counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexWeight {
    /// The name a person looks for.
    Title,
    /// A stable identity typed verbatim: an address, a site, a username.
    Identity,
    /// A secondary line: an author, a sender, a folder.
    Subtitle,
    /// Other short fields: labels, tags, a subject.
    Metadata,
    /// Short prose.
    Body,
}

impl IndexWeight {
    /// The ranker's weight for a match in a field of this weight.
    #[must_use]
    pub fn field_weight(self) -> super::FieldWeight {
        match self {
            Self::Title => super::FieldWeight::TITLE,
            Self::Identity => super::FieldWeight::IDENTITY,
            Self::Subtitle => super::FieldWeight::SUBTITLE,
            Self::Metadata => super::FieldWeight::METADATA,
            Self::Body => super::FieldWeight::BODY,
        }
    }
}

/// A published action a hit invokes: an action of a resource the app
/// publishes, by their ids.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct IndexAction {
    pub resource: String,
    pub action: String,
}

/// One search entry, as the system index keeps it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IndexEntry {
    /// Stable within the app's publication: the resource id, or the
    /// resource and action a hit invokes.
    pub key: String,
    /// The stable resource id a hit opens: the entry's anchor, else the
    /// resource of its action. Without an action (or once it is gone) a
    /// hit opens the app here through its `open` action.
    pub resource: String,
    /// The published action a hit invokes, when the entry names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<IndexAction>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// The result group the app lists the entry under ("Passwords").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// A named icon (a lucide kebab name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// A nudge over an equal match (a favourite), never an override.
    #[serde(default)]
    pub boost: i32,
    /// More short text the entry is found by, each with its weight.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<(String, IndexWeight)>,
    /// The entry describes a secret: found only while the app reports its
    /// secrets unlocked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,
}

/// `text` cut to at most `max` bytes at a character boundary.
fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        text.truncate(end);
    }
    text
}

impl IndexEntry {
    /// The entry within this module's bounds: texts cut, fields capped,
    /// empty fields dropped. `None` for an entry with nothing to find it by
    /// or nothing to open.
    #[must_use]
    pub fn bounded(self) -> Option<Self> {
        let title = cut(self.title.trim().to_owned(), MAX_TEXT_BYTES);
        if title.is_empty()
            || self.resource.is_empty()
            || self.key.is_empty()
            || self.resource.len() > MAX_ID_BYTES
            || self.key.len() > MAX_ID_BYTES
            || self.action.as_ref().is_some_and(|action| {
                [&action.resource, &action.action]
                    .into_iter()
                    .any(|id| id.is_empty() || id.len() > MAX_ID_BYTES)
            })
        {
            return None;
        }
        let short = |text: Option<String>| {
            text.map(|text| cut(text.trim().to_owned(), MAX_TEXT_BYTES))
                .filter(|text| !text.is_empty())
        };
        Some(Self {
            title,
            subtitle: short(self.subtitle),
            group: short(self.group),
            icon: short(self.icon),
            fields: self
                .fields
                .into_iter()
                .map(|(text, weight)| (cut(text.trim().to_owned(), MAX_TEXT_BYTES), weight))
                .filter(|(text, _)| !text.is_empty())
                .take(MAX_FIELDS)
                .collect(),
            ..self
        })
    }

    /// Every text the entry is found by, with its weight: its title, its
    /// secondary line and its fields.
    pub fn searchable(&self) -> impl Iterator<Item = (&str, IndexWeight)> {
        std::iter::once((self.title.as_str(), IndexWeight::Title))
            .chain(
                self.subtitle
                    .as_deref()
                    .map(|subtitle| (subtitle, IndexWeight::Subtitle)),
            )
            .chain(
                self.fields
                    .iter()
                    .map(|(text, weight)| (text.as_str(), *weight)),
            )
    }
}

/// A change to what one app connection published.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IndexDelta {
    /// Discard what this app published in its slot before applying the rest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reset: bool,
    /// Entries new or changed, by their key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upsert: Vec<IndexEntry>,
    /// Keys of entries no longer offered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    /// Whether the app's secrets are unlocked now; `None` says nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets_unlocked: Option<bool>,
}

impl IndexDelta {
    /// The delta as it crosses the broker.
    #[must_use]
    pub fn encode(&self) -> Option<Vec<u8>> {
        serde_json::to_vec(self).ok()
    }

    /// The delta `bytes` encode, within [`MAX_DELTA_BYTES`].
    ///
    /// # Errors
    ///
    /// The bytes are too many or not a delta.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_DELTA_BYTES {
            return Err(format!(
                "a search index delta takes at most {MAX_DELTA_BYTES} bytes"
            ));
        }
        serde_json::from_slice(bytes).map_err(|error| error.to_string())
    }
}

/// What an app's search source is of now, read every frame: a publication
/// is rebuilt only when it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexState {
    /// Changes whenever the source's entries may have.
    pub generation: u64,
    /// Whether the secrets behind secret-bearing entries are unlocked;
    /// `None` says nothing.
    pub secrets_unlocked: Option<bool>,
}

/// What one route last published, so only what changed is sent again.
#[derive(Debug, Default)]
pub struct Publisher {
    published: Option<HashMap<String, IndexEntry>>,
    state: Option<IndexState>,
}

impl Publisher {
    /// Forget what was sent: the next publication sends the whole set.
    pub fn reset(&mut self) {
        self.published = None;
        self.state = None;
    }

    /// Whether the source moved since the last publication.
    #[must_use]
    pub fn wants(&self, state: IndexState) -> bool {
        self.state != Some(state)
    }

    /// The deltas, each within [`MAX_DELTA_BYTES`], that bring the index
    /// from what this publisher sent to `entries` at `state`. What they
    /// carry counts as sent.
    pub fn publish(&mut self, state: IndexState, entries: Vec<IndexEntry>) -> Vec<IndexDelta> {
        let mut next: BTreeMap<String, IndexEntry> = BTreeMap::new();
        for entry in entries.into_iter().filter_map(IndexEntry::bounded) {
            if next.len() >= MAX_ENTRIES {
                break;
            }
            next.entry(entry.key.clone()).or_insert(entry);
        }
        let reset = self.published.is_none();
        let (upsert, remove): (Vec<IndexEntry>, Vec<String>) = match &self.published {
            None => (next.values().cloned().collect(), Vec::new()),
            Some(before) => (
                next.values()
                    .filter(|entry| before.get(&entry.key) != Some(*entry))
                    .cloned()
                    .collect(),
                {
                    let mut gone: Vec<String> = before
                        .keys()
                        .filter(|key| !next.contains_key(*key))
                        .cloned()
                        .collect();
                    gone.sort();
                    gone
                },
            ),
        };
        let secrets_moved = self
            .state
            .is_none_or(|before| before.secrets_unlocked != state.secrets_unlocked);
        self.published = Some(next.into_iter().collect());
        self.state = Some(state);
        if !reset && upsert.is_empty() && remove.is_empty() && !secrets_moved {
            return Vec::new();
        }
        chunked(IndexDelta {
            reset,
            upsert,
            remove,
            secrets_unlocked: state.secrets_unlocked,
        })
    }
}

/// The encoded length of `value`, for chunking.
fn encoded_len<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}

/// `delta` as deltas of at most [`MAX_DELTA_BYTES`] each, in order: the
/// first carries the reset, the removals and the lock.
fn chunked(delta: IndexDelta) -> Vec<IndexDelta> {
    // Room left for the envelope and the separators between entries.
    const SLACK: usize = 256;
    let IndexDelta {
        reset,
        upsert,
        remove,
        secrets_unlocked,
    } = delta;
    let mut current = IndexDelta {
        reset,
        upsert: Vec::new(),
        remove: Vec::new(),
        secrets_unlocked,
    };
    let mut used = encoded_len(&current).saturating_add(SLACK);
    let mut out = Vec::new();
    for key in remove {
        let len = encoded_len(&key).saturating_add(1);
        if used.saturating_add(len) > MAX_DELTA_BYTES && !current.remove.is_empty() {
            out.push(std::mem::take(&mut current));
            used = SLACK;
        }
        used = used.saturating_add(len);
        current.remove.push(key);
    }
    for entry in upsert {
        let len = encoded_len(&entry).saturating_add(1);
        if used.saturating_add(len) > MAX_DELTA_BYTES
            && !(current.upsert.is_empty() && current.remove.is_empty())
        {
            out.push(std::mem::take(&mut current));
            used = SLACK;
        }
        used = used.saturating_add(len);
        current.upsert.push(entry);
    }
    out.push(current);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, title: &str) -> IndexEntry {
        IndexEntry {
            key: key.into(),
            resource: key.into(),
            action: None,
            title: title.into(),
            subtitle: None,
            group: None,
            icon: None,
            boost: 0,
            fields: Vec::new(),
            secret: false,
        }
    }

    fn state(generation: u64) -> IndexState {
        IndexState {
            generation,
            secrets_unlocked: None,
        }
    }

    #[test]
    fn a_publisher_sends_the_whole_set_once_then_only_what_changed() {
        let mut publisher = Publisher::default();
        let first = publisher.publish(state(1), vec![entry("a", "Alpha"), entry("b", "Beta")]);
        assert_eq!(first.len(), 1);
        assert!(first[0].reset);
        assert_eq!(first[0].upsert.len(), 2);

        assert!(!publisher.wants(state(1)));
        let changed = publisher.publish(state(2), vec![entry("a", "Alpha 2"), entry("c", "Gamma")]);
        assert_eq!(changed.len(), 1);
        assert!(!changed[0].reset);
        let keys: Vec<&str> = changed[0]
            .upsert
            .iter()
            .map(|entry| entry.key.as_str())
            .collect();
        assert_eq!(keys, ["a", "c"]);
        assert_eq!(changed[0].remove, ["b"]);

        let same = publisher.publish(state(3), vec![entry("a", "Alpha 2"), entry("c", "Gamma")]);
        assert!(same.is_empty(), "nothing changed, nothing is sent");

        publisher.reset();
        let again = publisher.publish(state(3), vec![entry("a", "Alpha 2")]);
        assert!(again[0].reset, "a new route gets the whole set again");
    }

    #[test]
    fn a_lock_change_alone_is_published() {
        let mut publisher = Publisher::default();
        let unlocked = IndexState {
            generation: 1,
            secrets_unlocked: Some(true),
        };
        publisher.publish(unlocked, vec![entry("a", "Alpha")]);
        let locked = IndexState {
            secrets_unlocked: Some(false),
            ..unlocked
        };
        assert!(publisher.wants(locked));
        let deltas = publisher.publish(locked, vec![entry("a", "Alpha")]);
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].secrets_unlocked, Some(false));
        assert!(deltas[0].upsert.is_empty());
    }

    #[test]
    fn a_large_set_crosses_in_bounded_deltas() {
        let mut publisher = Publisher::default();
        let long = "x".repeat(MAX_TEXT_BYTES);
        let entries: Vec<IndexEntry> = (0..4_000)
            .map(|at| IndexEntry {
                subtitle: Some(long.clone()),
                ..entry(&format!("item-{at}"), &long)
            })
            .collect();
        let deltas = publisher.publish(state(1), entries);
        assert!(deltas.len() > 1);
        assert!(deltas[0].reset);
        assert!(deltas.iter().skip(1).all(|delta| !delta.reset));
        assert_eq!(
            deltas.iter().map(|delta| delta.upsert.len()).sum::<usize>(),
            4_000
        );
        for delta in &deltas {
            let bytes = delta.encode().expect("encodes");
            assert!(bytes.len() <= MAX_DELTA_BYTES);
            assert_eq!(IndexDelta::decode(&bytes).expect("decodes"), *delta);
        }
    }

    #[test]
    fn entries_are_bounded_and_need_something_to_open() {
        let long = "é".repeat(MAX_TEXT_BYTES);
        let bounded = IndexEntry {
            fields: (0..100)
                .map(|at| (format!("field {at}"), IndexWeight::Metadata))
                .collect(),
            ..entry("a", &long)
        }
        .bounded()
        .expect("kept");
        assert!(bounded.title.len() <= MAX_TEXT_BYTES);
        assert_eq!(bounded.fields.len(), MAX_FIELDS);
        assert!(entry("", "Nothing to open").bounded().is_none());
        assert!(entry("a", "   ").bounded().is_none());
    }
}
