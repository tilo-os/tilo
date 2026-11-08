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
//! Every text is the one the session's person reads: text derived from rich
//! text reads each reference as it reads for them (a resource they hold by
//! its title now, one they cannot read as not shared,
//! `tilo_reference::embed::read_text`), never as the run it was written
//! with, so the index never matches a query against a title they cannot
//! read.
//!
//! The publication is incremental: a [`Publisher`] remembers what its
//! connection last sent and sends an [`IndexDelta`] of what changed, the
//! whole set again (`reset`) only on a new route.
//!
//! The types live here, in the toolkit-free resource layer, because both
//! halves that publish to the one index read them: a toolkit app's window
//! (`tilo_ui`) and a tilolang app's instance, whose host links no widget.

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

/// A published action a hit invokes: an action of a resource the app
/// publishes, by their ids.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct IndexAction {
    pub resource: String,
    pub action: String,
}

/// What kind of thing an entry is, where the app keeps something the desk's
/// generic rows would read poorly: each is its own section, drawn as what it
/// is rather than as a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexKind {
    /// An agent of the workspace, opened in the conversation it answers in.
    Agent,
    /// A conversation: a channel, a group or a direct message.
    Conversation,
    /// One message in a conversation, opened where it sits.
    Message,
    /// A person or an agent the workspace knows, drawn as themselves.
    Person,
}

/// A person or an agent an entry is about: who a row shows and what it says
/// under its name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct IndexPerson {
    /// The name the record gives them, when it can say it; empty: what their
    /// `anchor` is called now.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The resource that names them when the record only holds their id
    /// (`chat.workspace/people/…`), empty when there is none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub anchor: String,
    /// Their stable id: picks their colour and their picture.
    pub identity: String,
    /// Whether they are an agent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub agent: bool,
}

/// One search entry, as the system index keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// What kind of thing it is, when the app keeps something the desk's
    /// generic rows would read poorly (an agent, a conversation, a message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<IndexKind>,
    /// Who the row shows, in the order people read them: the first is its
    /// avatar, the rest its faces. The sender of a message, the people in a
    /// conversation, the agent itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<IndexPerson>,
    /// Who keeps it, for an agent: the row says so, and "you" when it is the
    /// person searching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keeper: Option<IndexPerson>,
    /// Where it is, for a message: the conversation it sits in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<IndexPerson>,
    /// When it happened, in seconds since the Unix epoch: the row says how
    /// long ago.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<i64>,
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
    /// What its text refers to, each in the one form a reference in rich
    /// text is written in (`tilo_reference::embed`): the system index finds
    /// the entry by what each is called now, not by the title written into
    /// the text, and answers which entries refer to a resource (its
    /// backlinks).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
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
        let person = |person: IndexPerson| {
            (!person.identity.is_empty() && person.identity.len() <= MAX_ID_BYTES).then(|| {
                IndexPerson {
                    name: cut(person.name.trim().to_owned(), MAX_TEXT_BYTES),
                    anchor: cut(person.anchor.trim().to_owned(), MAX_ID_BYTES),
                    ..person
                }
            })
        };
        Some(Self {
            title,
            subtitle: short(self.subtitle),
            group: short(self.group),
            icon: short(self.icon),
            people: self
                .people
                .into_iter()
                .filter_map(person)
                .take(MAX_FIELDS)
                .collect(),
            keeper: self.keeper.and_then(person),
            place: self.place.and_then(person),
            fields: self
                .fields
                .into_iter()
                .map(|(text, weight)| (cut(text.trim().to_owned(), MAX_TEXT_BYTES), weight))
                .filter(|(text, _)| !text.is_empty())
                .take(MAX_FIELDS)
                .collect(),
            references: self
                .references
                .into_iter()
                .filter(|reference| !reference.is_empty() && reference.len() <= MAX_ID_BYTES)
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

    /// The deltas, each within [`MAX_DELTA_BYTES`], that bring the index from what this
    /// publisher sent to `entries` at `state`. What they carry counts as sent.
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
                    gone.sort_unstable();
                    gone
                },
            ),
        };
        self.published = Some(next.into_iter().collect());
        self.delta(state, reset, upsert, remove)
    }

    /// The deltas that send `upsert` entries, new or changed, and drop `remove` keys, from what
    /// this publisher sent: what a source that knows what moved publishes instead of reading
    /// its whole set again. `None` when what this publisher sent does not cover the change (the
    /// first publication on a route, or a source that never sent the whole set): the caller
    /// publishes the whole set again instead.
    pub fn publish_moved(
        &mut self,
        state: IndexState,
        upsert: Vec<IndexEntry>,
        remove: Vec<String>,
    ) -> Option<Vec<IndexDelta>> {
        let published = self.published.as_mut()?;
        let upsert: Vec<IndexEntry> = upsert
            .into_iter()
            .filter_map(IndexEntry::bounded)
            .filter(|entry| published.get(&entry.key) != Some(entry))
            .collect();
        // The set the whole publication kept has room; one that would grow past it is read
        // whole again, which caps it.
        let added = upsert
            .iter()
            .filter(|entry| !published.contains_key(&entry.key))
            .count();
        if published.len().saturating_add(added) > MAX_ENTRIES {
            return None;
        }
        for entry in &upsert {
            published.insert(entry.key.clone(), entry.clone());
        }
        let mut remove: Vec<String> = remove
            .into_iter()
            .filter(|key| published.remove(key).is_some())
            .collect();
        remove.sort_unstable();
        remove.dedup();
        Some(self.delta(state, false, upsert, remove))
    }

    /// The deltas `upsert` and `remove` make at `state`, once they count as sent.
    fn delta(
        &mut self,
        state: IndexState,
        reset: bool,
        upsert: Vec<IndexEntry>,
        remove: Vec<String>,
    ) -> Vec<IndexDelta> {
        let secrets_moved = self
            .state
            .is_none_or(|before| before.secrets_unlocked != state.secrets_unlocked);
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
            references: Vec::new(),
            ..IndexEntry::default()
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
