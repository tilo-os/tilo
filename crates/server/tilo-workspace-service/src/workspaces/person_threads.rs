//! Per-person classification of what individual threads are about, kept on this
//! server separate from who someone is (their private contact groups).
//!
//! Who a person is (for example `Investors` or `Customers`) is durable identity
//! that belongs in private contacts. What one thread or message is about can
//! vary per conversation — a sender in both `Investors` and `Customers` may
//! write about fundraising today and a product question tomorrow, or an unknown
//! sender may write about an investment inquiry.
//!
//! This module keeps a bounded per-person store (newest [`MAX_THREAD_TOPICS`]
//! entries, written atomically with `0o600` permissions) holding each thread's
//! topic classification, its provenance, strict override authority
//! (`Person > LargeModel > DecisionModel`), and an optional source-item
//! correlation key with the ids of overlapping automations that fired on it.
//!
//! Untrusted inbound content may tag a thread, never promote a sender into a
//! durable contact group without the person's own word.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;

use super::Workspaces;

/// Maximum number of per-thread topic entries kept per person (newest kept).
pub const MAX_THREAD_TOPICS: usize = 500;

/// Fallback choice appended when narrowing options to a multi-group sender's
/// known contact groups.
pub const NEITHER_OPTION: &str = "neither";

/// Fallback choice appended when classifying an unknown sender across all
/// event options.
pub const NONE_OPTION: &str = "none";

/// Subfolder inside a person's server store where thread topics are kept.
const FOLDER: &str = "threads";
/// File name inside [`FOLDER`].
const FILE_NAME: &str = "topics.json";
/// On-disk schema version.
const SCHEMA_VERSION: u32 = 1;
/// Hard byte cap for the serialized thread-topic file.
const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Maximum byte length for short identifier and topic fields.
const MAX_FIELD_BYTES: usize = 256;
/// Maximum byte length for provenance source descriptions.
const MAX_SOURCE_BYTES: usize = 512;
/// Maximum correlated event ids tracked per thread entry.
const MAX_CORRELATED_EVENTS: usize = 64;
/// Maximum secondary multi-label topics tracked per thread entry.
const MAX_SECONDARY_TOPICS: usize = 16;

/// Serializes read-modify-write updates to thread topic files on this server.
static WRITING: Mutex<()> = Mutex::new(());

/// Who vouches for a thread's topic classification, ordered strictly from
/// weakest to strongest: `DecisionModel < LargeModel < Person`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum TopicAuthority {
    /// Classified by the fast decision model (weakest).
    DecisionModel,
    /// Classified or refined by the large model.
    #[default]
    LargeModel,
    /// Set or corrected by the person themselves (strongest, never overwritten
    /// by a model).
    Person,
}

impl TopicAuthority {
    /// Whether an update carrying `self` authority may overwrite or remove an
    /// existing classification held at `existing`.
    #[must_use]
    pub const fn may_overwrite(self, existing: Self) -> bool {
        self.rank() >= existing.rank()
    }

    /// Whether this authority represents the person's own word.
    #[must_use]
    pub const fn is_person(self) -> bool {
        matches!(self, Self::Person)
    }

    /// Whether this authority may promote a sender into a durable contact
    /// group. Untrusted inbound mail or model guesses may tag a thread, never
    /// add someone to a durable contact group without the person's own nod.
    #[must_use]
    pub const fn may_promote_contact_group(self) -> bool {
        matches!(self, Self::Person)
    }

    const fn rank(self) -> u8 {
        match self {
            Self::DecisionModel => 0,
            Self::LargeModel => 1,
            Self::Person => 2,
        }
    }
}

/// Stored topic classification for one thread belonging to one person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTopic {
    /// Stable thread identifier (for example a mail thread or conversation id).
    pub thread_id: String,
    /// Primary classified topic for this thread.
    pub topic: String,
    /// Additional topics when a multi-label check detects more than one topic
    /// on the same thread.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary_topics: Vec<String>,
    /// Where this classification came from (for example `"contact-group"`,
    /// `"decision-model"`, `"large-model"`, or `"person-correction"`).
    pub source: String,
    /// Authority level governing overwrite protection (`Person > LargeModel > DecisionModel`).
    pub authority: TopicAuthority,
    /// Timestamp of the last classification or update in Unix milliseconds.
    pub updated_at_ms: i64,
    /// Optional canonical source-item correlation key linking overlapping
    /// automations triggered by the same incoming item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_key: Option<String>,
    /// Event ids correlated with this thread/source item, deduplicated in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub correlated_events: Vec<String>,
}

impl ThreadTopic {
    /// Build a new thread topic entry with the required fields.
    #[must_use]
    pub fn new(
        thread_id: impl Into<String>,
        topic: impl Into<String>,
        source: impl Into<String>,
        authority: TopicAuthority,
        updated_at_ms: i64,
    ) -> Self {
        Self {
            thread_id: thread_id.into(),
            topic: topic.into(),
            secondary_topics: Vec::new(),
            source: source.into(),
            authority,
            updated_at_ms,
            correlation_key: None,
            correlated_events: Vec::new(),
        }
    }

    /// Attach a source-item correlation key for coordinating overlapping
    /// automations.
    #[must_use]
    pub fn with_correlation_key(mut self, correlation_key: impl Into<String>) -> Self {
        self.correlation_key = Some(correlation_key.into());
        self
    }

    /// Attach correlated event ids to this entry.
    #[must_use]
    pub fn with_correlated_events(
        mut self,
        events: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.correlated_events = events.into_iter().map(Into::into).collect();
        self
    }

    /// Attach secondary topics detected alongside the primary topic.
    #[must_use]
    pub fn with_secondary_topics(
        mut self,
        topics: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.secondary_topics = topics.into_iter().map(Into::into).collect();
        self
    }

    /// All topics assigned to this thread: primary first, then any distinct
    /// secondary topics.
    #[must_use]
    pub fn all_topics(&self) -> Vec<&str> {
        let mut all = vec![self.topic.as_str()];
        for extra in &self.secondary_topics {
            if !all
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(extra))
            {
                all.push(extra.as_str());
            }
        }
        all
    }
}

/// Errors returned when validating or persisting thread topic entries.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TopicStoreError {
    /// A required field was blank, contained control characters, or exceeded
    /// its length bound.
    #[error("invalid thread topic entry: {0}")]
    Invalid(String),
    /// A weaker authority attempted to overwrite or remove an entry held at a
    /// stronger authority (for example a model attempting to overwrite a
    /// person's correction).
    #[error(
        "cannot overwrite thread `{thread_id}` held at {existing:?} authority with {attempted:?}"
    )]
    Outranked {
        thread_id: String,
        existing: TopicAuthority,
        attempted: TopicAuthority,
    },
    /// Underlying disk read or atomic write failed.
    #[error("thread topic storage failed: {0}")]
    Storage(String),
}

/// Result of separating who a sender is (their private contact groups) from
/// what one thread is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopicSelection {
    /// Sender belongs to a single group matching the event's options and the
    /// event only asks who it is: resolved deterministically with zero model
    /// tokens.
    Deterministic(String),
    /// A decision model must classify what this specific thread is about.
    ///
    /// - For a multi-group sender whose groups overlap `event_options`,
    ///   `options` is narrowed to those overlapping groups plus `"neither"`.
    /// - For an unknown sender (or one whose groups do not overlap
    ///   `event_options`), `options` contains all `event_options` plus `"none"`.
    AskModel {
        /// Complete choice list including the `"neither"` or `"none"` fallback.
        options: Vec<String>,
        /// Candidate topic labels without the fallback, suitable for batched
        /// independent yes/no questions.
        candidates: Vec<String>,
        /// `true` when options were narrowed from the sender's known groups.
        narrowed: bool,
    },
}

impl TopicSelection {
    /// Whether resolving this thread requires calling a model.
    #[must_use]
    pub const fn needs_model(&self) -> bool {
        matches!(self, Self::AskModel { .. })
    }

    /// The zero-token deterministic topic, if no model call is needed.
    #[must_use]
    pub fn deterministic_topic(&self) -> Option<&str> {
        match self {
            Self::Deterministic(topic) => Some(topic.as_str()),
            Self::AskModel { .. } => None,
        }
    }

    /// Choice options to offer the decision model, or an empty slice when
    /// resolved deterministically.
    #[must_use]
    pub fn options(&self) -> &[String] {
        match self {
            Self::Deterministic(_) => &[],
            Self::AskModel { options, .. } => options.as_slice(),
        }
    }

    /// Candidate topic labels (excluding `"neither"` / `"none"`), or an empty
    /// slice when resolved deterministically.
    #[must_use]
    pub fn candidates(&self) -> &[String] {
        match self {
            Self::Deterministic(_) => &[],
            Self::AskModel { candidates, .. } => candidates.as_slice(),
        }
    }
}

/// Decide how to classify a thread given the sender's private contact groups
/// (`sender_groups`) and the candidate options an event cares about
/// (`event_options`), assuming the event only asks who the sender is.
///
/// - 1 group matching `event_options` (or when `event_options` is empty) ->
///   [`TopicSelection::Deterministic`] with zero model tokens.
/// - Multiple groups -> narrow choice options to those matching groups plus
///   [`NEITHER_OPTION`] (`"neither"`).
/// - Unknown sender (or single group outside `event_options`) -> all
///   `event_options` plus [`NONE_OPTION`] (`"none"`).
#[must_use]
pub fn topic_options_for_sender<S: AsRef<str>, E: AsRef<str>>(
    sender_groups: &[S],
    event_options: &[E],
) -> TopicSelection {
    topic_options_for_sender_with_question(sender_groups, event_options, true)
}

/// Like [`topic_options_for_sender`], with `asks_only_who` specifying whether
/// the event only asks who the sender is (`true`) or asks an additional
/// per-thread topic question (`false`).
///
/// When `asks_only_who` is `false`, even a single-group sender is sent to the
/// decision model narrowed to that group plus `"neither"`, because knowing who
/// someone is does not answer a further question about this specific message.
#[must_use]
pub fn topic_options_for_sender_with_question<S: AsRef<str>, E: AsRef<str>>(
    sender_groups: &[S],
    event_options: &[E],
    asks_only_who: bool,
) -> TopicSelection {
    let groups = normalize_labels(sender_groups);
    let events = normalize_labels(event_options);

    if groups.is_empty() {
        return ask_all_with_fallback(events, NONE_OPTION, false);
    }

    let matched: Vec<String> = if events.is_empty() {
        groups.clone()
    } else {
        events
            .iter()
            .filter(|opt| groups.iter().any(|group| group.eq_ignore_ascii_case(opt)))
            .cloned()
            .collect()
    };

    if groups.len() == 1 {
        if let Some(single) = matched.first()
            && matched.len() == 1
        {
            if asks_only_who {
                return TopicSelection::Deterministic(single.clone());
            }
            return ask_all_with_fallback(vec![single.clone()], NEITHER_OPTION, true);
        }
        return ask_all_with_fallback(events, NONE_OPTION, false);
    }

    if !matched.is_empty() {
        return ask_all_with_fallback(matched, NEITHER_OPTION, true);
    }

    ask_all_with_fallback(events, NONE_OPTION, false)
}

fn ask_all_with_fallback(
    candidates: Vec<String>,
    fallback: &str,
    narrowed: bool,
) -> TopicSelection {
    let mut options = candidates.clone();
    if !options
        .iter()
        .any(|item| item.eq_ignore_ascii_case(fallback))
    {
        options.push(fallback.to_owned());
    }
    TopicSelection::AskModel {
        options,
        candidates,
        narrowed,
    }
}

fn normalize_labels<T: AsRef<str>>(raw: &[T]) -> Vec<String> {
    let mut out = Vec::new();
    for item in raw {
        let trimmed = item.as_ref().trim();
        if trimmed.is_empty() {
            continue;
        }
        if !out
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(trimmed))
        {
            out.push(trimmed.to_owned());
        }
    }
    out
}

/// On-disk envelope for one person's bounded thread-topic store.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    entries: Vec<ThreadTopic>,
}

const fn default_version() -> u32 {
    SCHEMA_VERSION
}

/// Per-person bounded store of thread topic classifications on disk.
#[derive(Clone, Debug)]
pub struct ThreadTopicStore {
    path: PathBuf,
    max_entries: usize,
}

impl ThreadTopicStore {
    /// Open or prepare a store at `path` bounded to [`MAX_THREAD_TOPICS`]
    /// newest entries.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            max_entries: MAX_THREAD_TOPICS,
        }
    }

    /// Open or prepare a store at `path` with a custom entry bound (clamped to
    /// `1..=MAX_THREAD_TOPICS`).
    #[must_use]
    pub fn with_capacity(path: PathBuf, max_entries: usize) -> Self {
        Self {
            path,
            max_entries: max_entries.clamp(1, MAX_THREAD_TOPICS),
        }
    }

    /// Path to the backing JSON file on disk.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// List all stored thread topics, newest first.
    ///
    /// # Errors
    /// Returns an error if the store file cannot be read or decoded.
    pub fn list(&self) -> Result<Vec<ThreadTopic>, TopicStoreError> {
        let mut entries = self.read_entries()?;
        sort_newest_first(&mut entries);
        Ok(entries)
    }

    /// Look up one thread's stored classification by `thread_id`.
    ///
    /// # Errors
    /// Returns an error if the store file cannot be read or decoded.
    pub fn get(&self, thread_id: &str) -> Result<Option<ThreadTopic>, TopicStoreError> {
        let wanted = thread_id.trim();
        if wanted.is_empty() {
            return Ok(None);
        }
        let entries = self.read_entries()?;
        Ok(entries.into_iter().find(|entry| entry.thread_id == wanted))
    }

    /// Look up all stored thread entries sharing `correlation_key`, newest
    /// first.
    ///
    /// # Errors
    /// Returns an error if the store file cannot be read or decoded.
    pub fn by_correlation_key(
        &self,
        correlation_key: &str,
    ) -> Result<Vec<ThreadTopic>, TopicStoreError> {
        let wanted = correlation_key.trim();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let mut matched: Vec<ThreadTopic> = self
            .read_entries()?
            .into_iter()
            .filter(|entry| entry.correlation_key.as_deref() == Some(wanted))
            .collect();
        sort_newest_first(&mut matched);
        Ok(matched)
    }

    /// Insert or update a thread's topic classification.
    ///
    /// Enforces `Person > LargeModel > DecisionModel`: an existing entry held
    /// at a strictly stronger authority cannot be overwritten by a weaker
    /// authority. When updating an existing entry, correlated event ids are
    /// merged without duplicates. The store is trimmed to the newest
    /// `max_entries` entries and written atomically with `0o600` permissions.
    ///
    /// # Errors
    /// Returns [`TopicStoreError::Invalid`] for malformed fields,
    /// [`TopicStoreError::Outranked`] when authority is too weak, or
    /// [`TopicStoreError::Storage`] on I/O failure.
    pub fn upsert(&self, entry: ThreadTopic) -> Result<ThreadTopic, TopicStoreError> {
        let mut incoming = sanitize_entry(entry)?;
        let _guard = WRITING.lock_recover();
        let mut map = self.read_map()?;

        if let Some(existing) = map.get(&incoming.thread_id) {
            if !incoming.authority.may_overwrite(existing.authority) {
                return Err(TopicStoreError::Outranked {
                    thread_id: incoming.thread_id,
                    existing: existing.authority,
                    attempted: incoming.authority,
                });
            }
            if incoming.correlation_key.is_none() {
                incoming.correlation_key = existing.correlation_key.clone();
            }
            let mut merged_events = existing.correlated_events.clone();
            for event_id in &incoming.correlated_events {
                push_unique_bounded(&mut merged_events, event_id, MAX_CORRELATED_EVENTS);
            }
            incoming.correlated_events = merged_events;
        }

        let stored = incoming.clone();
        map.insert(incoming.thread_id.clone(), incoming);
        self.write_map(map)?;
        Ok(stored)
    }

    /// Attach an overlapping `event_id` (and optional `correlation_key`) to an
    /// existing thread entry without altering its topic or authority.
    ///
    /// Returns `Ok(None)` if `thread_id` is not yet stored.
    ///
    /// # Errors
    /// Returns [`TopicStoreError::Invalid`] if `event_id` or `correlation_key`
    /// is invalid, or [`TopicStoreError::Storage`] on I/O failure.
    pub fn correlate_event(
        &self,
        thread_id: &str,
        correlation_key: Option<&str>,
        event_id: &str,
        updated_at_ms: i64,
    ) -> Result<Option<ThreadTopic>, TopicStoreError> {
        let wanted_thread = clean_required(thread_id, "threadId", MAX_FIELD_BYTES)?;
        let wanted_event = clean_required(event_id, "eventId", MAX_FIELD_BYTES)?;
        let wanted_key = match correlation_key {
            Some(key) => Some(clean_required(key, "correlationKey", MAX_FIELD_BYTES)?),
            None => None,
        };

        let _guard = WRITING.lock_recover();
        let mut map = self.read_map()?;
        let Some(existing) = map.get_mut(&wanted_thread) else {
            return Ok(None);
        };
        if let Some(key) = wanted_key {
            existing.correlation_key = Some(key);
        }
        push_unique_bounded(
            &mut existing.correlated_events,
            &wanted_event,
            MAX_CORRELATED_EVENTS,
        );
        if updated_at_ms > existing.updated_at_ms {
            existing.updated_at_ms = updated_at_ms;
        }
        let updated = existing.clone();
        self.write_map(map)?;
        Ok(Some(updated))
    }

    /// Remove a stored thread classification if `authority` is allowed to
    /// overwrite the existing entry's authority.
    ///
    /// Returns `Ok(true)` if removed, `Ok(false)` if no entry existed.
    ///
    /// # Errors
    /// Returns [`TopicStoreError::Outranked`] when a weaker authority tries to
    /// remove a stronger classification, or [`TopicStoreError::Storage`] on
    /// I/O failure.
    pub fn remove(
        &self,
        thread_id: &str,
        authority: TopicAuthority,
    ) -> Result<bool, TopicStoreError> {
        let wanted = thread_id.trim();
        if wanted.is_empty() {
            return Ok(false);
        }
        let _guard = WRITING.lock_recover();
        let mut map = self.read_map()?;
        let Some(existing) = map.get(wanted) else {
            return Ok(false);
        };
        if !authority.may_overwrite(existing.authority) {
            return Err(TopicStoreError::Outranked {
                thread_id: wanted.to_owned(),
                existing: existing.authority,
                attempted: authority,
            });
        }
        map.remove(wanted);
        self.write_map(map)?;
        Ok(true)
    }

    fn read_entries(&self) -> Result<Vec<ThreadTopic>, TopicStoreError> {
        let metadata = match std::fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(TopicStoreError::Storage(error.to_string())),
        };
        if metadata.len() > MAX_BYTES {
            return Err(TopicStoreError::Storage(
                "thread topic store exceeds maximum size".to_owned(),
            ));
        }
        let bytes = std::fs::read(&self.path)
            .map_err(|error| TopicStoreError::Storage(error.to_string()))?;
        let file: StoreFile = serde_json::from_slice(&bytes)
            .map_err(|error| TopicStoreError::Storage(error.to_string()))?;
        Ok(file.entries)
    }

    fn read_map(&self) -> Result<BTreeMap<String, ThreadTopic>, TopicStoreError> {
        let mut map = BTreeMap::new();
        for entry in self.read_entries()? {
            map.insert(entry.thread_id.clone(), entry);
        }
        Ok(map)
    }

    fn write_map(&self, map: BTreeMap<String, ThreadTopic>) -> Result<(), TopicStoreError> {
        let mut entries: Vec<ThreadTopic> = map.into_values().collect();
        sort_newest_first(&mut entries);
        entries.truncate(self.max_entries);
        let file = StoreFile {
            version: SCHEMA_VERSION,
            entries,
        };
        let bytes = serde_json::to_vec(&file)
            .map_err(|error| TopicStoreError::Storage(error.to_string()))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BYTES {
            return Err(TopicStoreError::Storage(
                "serialized thread topic store exceeds maximum size".to_owned(),
            ));
        }
        tilo_files::native::atomic::write_creating_parent_with_mode(&self.path, &bytes, 0o600)
            .map_err(|error| TopicStoreError::Storage(error.to_string()))
    }
}

fn sort_newest_first(entries: &mut [ThreadTopic]) {
    entries.sort_by(|left, right| {
        (Reverse(left.updated_at_ms), left.thread_id.as_str())
            .cmp(&(Reverse(right.updated_at_ms), right.thread_id.as_str()))
    });
}

fn clean_required(value: &str, field: &str, max_bytes: usize) -> Result<String, TopicStoreError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(TopicStoreError::Invalid(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max_bytes {
        return Err(TopicStoreError::Invalid(format!(
            "{field} exceeds {max_bytes} bytes"
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(TopicStoreError::Invalid(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(trimmed.to_owned())
}

fn sanitize_entry(entry: ThreadTopic) -> Result<ThreadTopic, TopicStoreError> {
    let thread_id = clean_required(&entry.thread_id, "threadId", MAX_FIELD_BYTES)?;
    let topic = clean_required(&entry.topic, "topic", MAX_FIELD_BYTES)?;
    let source = clean_required(&entry.source, "source", MAX_SOURCE_BYTES)?;
    let correlation_key = match entry.correlation_key {
        Some(key) if !key.trim().is_empty() => {
            Some(clean_required(&key, "correlationKey", MAX_FIELD_BYTES)?)
        }
        _ => None,
    };

    let mut secondary_topics = Vec::new();
    for raw in entry.secondary_topics {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(&topic) {
            continue;
        }
        let cleaned = clean_required(trimmed, "secondaryTopics", MAX_FIELD_BYTES)?;
        if !secondary_topics
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&cleaned))
            && secondary_topics.len() < MAX_SECONDARY_TOPICS
        {
            secondary_topics.push(cleaned);
        }
    }

    let mut correlated_events = Vec::new();
    for raw in entry.correlated_events {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let cleaned = clean_required(trimmed, "correlatedEvents", MAX_FIELD_BYTES)?;
        push_unique_bounded(&mut correlated_events, &cleaned, MAX_CORRELATED_EVENTS);
    }

    Ok(ThreadTopic {
        thread_id,
        topic,
        secondary_topics,
        source,
        authority: entry.authority,
        updated_at_ms: entry.updated_at_ms,
        correlation_key,
        correlated_events,
    })
}

fn push_unique_bounded(list: &mut Vec<String>, item: &str, max_len: usize) {
    if list.iter().any(|existing| existing == item) {
        return;
    }
    if list.len() >= max_len {
        list.remove(0);
    }
    list.push(item.to_owned());
}

impl Workspaces {
    /// Handle to `person`'s bounded per-thread topic store on this server.
    #[must_use]
    pub fn thread_topics(&self, person: &[u8; 32]) -> ThreadTopicStore {
        ThreadTopicStore::at(
            self.person_settings_root(person)
                .join(FOLDER)
                .join(FILE_NAME),
        )
    }

    /// Look up one thread's stored classification for `person`.
    ///
    /// # Errors
    /// Returns an error if the store cannot be read or decoded.
    pub fn thread_topic(
        &self,
        person: &[u8; 32],
        thread_id: &str,
    ) -> Result<Option<ThreadTopic>, TopicStoreError> {
        self.thread_topics(person).get(thread_id)
    }

    /// Upsert one thread's topic classification for `person`, enforcing
    /// `Person > LargeModel > DecisionModel` and the 500-entry bound.
    ///
    /// # Errors
    /// Returns [`TopicStoreError`] on validation failure, authority refusal, or
    /// I/O error.
    pub fn set_thread_topic(
        &self,
        person: &[u8; 32],
        entry: ThreadTopic,
    ) -> Result<ThreadTopic, TopicStoreError> {
        self.thread_topics(person).upsert(entry)
    }

    /// List all stored thread topics for `person`, newest first.
    ///
    /// # Errors
    /// Returns an error if the store cannot be read or decoded.
    pub fn list_thread_topics(
        &self,
        person: &[u8; 32],
    ) -> Result<Vec<ThreadTopic>, TopicStoreError> {
        self.thread_topics(person).list()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> (tempfile::TempDir, ThreadTopicStore) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ThreadTopicStore::at(dir.path().join(name).join(FILE_NAME));
        (dir, store)
    }

    #[test]
    fn single_group_matching_event_option_is_deterministic_zero_token() {
        let chosen = topic_options_for_sender(&["Investors"], &["Investors", "Customers"]);
        assert_eq!(chosen, TopicSelection::Deterministic("Investors".into()));
        assert!(!chosen.needs_model());
        assert_eq!(chosen.deterministic_topic(), Some("Investors"));
        assert!(chosen.options().is_empty());

        // Case-insensitive match preserves the event option's canonical casing.
        let canonical = topic_options_for_sender(&["investors"], &["Investors", "Customers"]);
        assert_eq!(canonical, TopicSelection::Deterministic("Investors".into()));

        // Empty event options with a single sender group also resolve directly.
        let open = topic_options_for_sender(&["Investors"], &[] as &[&str]);
        assert_eq!(open, TopicSelection::Deterministic("Investors".into()));
    }

    #[test]
    fn single_group_when_event_asks_more_than_identity_still_consults_model() {
        let chosen = topic_options_for_sender_with_question(
            &["Investors"],
            &["Investors", "Customers"],
            false,
        );
        assert_eq!(
            chosen,
            TopicSelection::AskModel {
                options: vec!["Investors".into(), "neither".into()],
                candidates: vec!["Investors".into()],
                narrowed: true,
            }
        );
        assert!(chosen.needs_model());
    }

    #[test]
    fn multiple_groups_narrow_options_to_matching_groups_plus_neither() {
        let chosen = topic_options_for_sender(
            &["Investors", "Customers"],
            &["Investors", "Customers", "Hiring"],
        );
        assert_eq!(
            chosen,
            TopicSelection::AskModel {
                options: vec!["Investors".into(), "Customers".into(), "neither".into()],
                candidates: vec!["Investors".into(), "Customers".into()],
                narrowed: true,
            }
        );

        // Multi-group sender with one matching group and one non-matching group
        // still asks the model (with "neither") so a personal mail is not
        // misclassified as an investor mail.
        let mixed =
            topic_options_for_sender(&["Investors", "Friends"], &["Investors", "Customers"]);
        assert_eq!(
            mixed,
            TopicSelection::AskModel {
                options: vec!["Investors".into(), "neither".into()],
                candidates: vec!["Investors".into()],
                narrowed: true,
            }
        );

        // Multi-group sender with empty event options narrows to all their groups + neither.
        let open = topic_options_for_sender(&["Investors", "Customers"], &[] as &[&str]);
        assert_eq!(
            open,
            TopicSelection::AskModel {
                options: vec!["Investors".into(), "Customers".into(), "neither".into()],
                candidates: vec!["Investors".into(), "Customers".into()],
                narrowed: true,
            }
        );
    }

    #[test]
    fn unknown_sender_returns_all_event_options_plus_none() {
        let chosen =
            topic_options_for_sender(&[] as &[&str], &["Investors", "Customers", "Hiring"]);
        assert_eq!(
            chosen,
            TopicSelection::AskModel {
                options: vec![
                    "Investors".into(),
                    "Customers".into(),
                    "Hiring".into(),
                    "none".into(),
                ],
                candidates: vec!["Investors".into(), "Customers".into(), "Hiring".into()],
                narrowed: false,
            }
        );
        assert!(chosen.needs_model());
    }

    #[test]
    fn authority_order_prevents_model_overwriting_person_or_stronger_model() {
        assert!(TopicAuthority::Person > TopicAuthority::LargeModel);
        assert!(TopicAuthority::LargeModel > TopicAuthority::DecisionModel);
        assert!(TopicAuthority::Person.may_promote_contact_group());
        assert!(!TopicAuthority::LargeModel.may_promote_contact_group());
        assert!(!TopicAuthority::DecisionModel.may_promote_contact_group());

        let (_dir, store) = temp_store("authority");

        // 1. DecisionModel writes initial topic.
        let initial = store
            .upsert(ThreadTopic::new(
                "thread-1",
                "Customers",
                "decision-model",
                TopicAuthority::DecisionModel,
                1_000,
            ))
            .expect("decision write");
        assert_eq!(initial.topic, "Customers");

        // 2. LargeModel can upgrade DecisionModel.
        let upgraded = store
            .upsert(ThreadTopic::new(
                "thread-1",
                "Investors",
                "large-model",
                TopicAuthority::LargeModel,
                2_000,
            ))
            .expect("large model upgrade");
        assert_eq!(upgraded.topic, "Investors");
        assert_eq!(upgraded.authority, TopicAuthority::LargeModel);

        // 3. DecisionModel cannot overwrite LargeModel.
        let denied_small = store.upsert(ThreadTopic::new(
            "thread-1",
            "Customers",
            "decision-model",
            TopicAuthority::DecisionModel,
            3_000,
        ));
        assert_eq!(
            denied_small,
            Err(TopicStoreError::Outranked {
                thread_id: "thread-1".into(),
                existing: TopicAuthority::LargeModel,
                attempted: TopicAuthority::DecisionModel,
            })
        );

        // 4. Person overrides LargeModel.
        let corrected = store
            .upsert(ThreadTopic::new(
                "thread-1",
                "Hiring",
                "person-correction",
                TopicAuthority::Person,
                4_000,
            ))
            .expect("person override");
        assert_eq!(corrected.topic, "Hiring");
        assert_eq!(corrected.authority, TopicAuthority::Person);

        // 5. Neither LargeModel nor DecisionModel can overwrite or remove Person's word.
        for weaker in [TopicAuthority::DecisionModel, TopicAuthority::LargeModel] {
            let err = store.upsert(ThreadTopic::new(
                "thread-1",
                "Investors",
                "model-guess",
                weaker,
                5_000,
            ));
            assert_eq!(
                err,
                Err(TopicStoreError::Outranked {
                    thread_id: "thread-1".into(),
                    existing: TopicAuthority::Person,
                    attempted: weaker,
                })
            );
            let remove_err = store.remove("thread-1", weaker);
            assert_eq!(
                remove_err,
                Err(TopicStoreError::Outranked {
                    thread_id: "thread-1".into(),
                    existing: TopicAuthority::Person,
                    attempted: weaker,
                })
            );
        }

        // Stored value remains the person's correction.
        let read_back = store
            .get("thread-1")
            .expect("read")
            .expect("thread present");
        assert_eq!(read_back.topic, "Hiring");
        assert_eq!(read_back.authority, TopicAuthority::Person);
    }

    #[test]
    fn store_bounds_to_newest_500_entries_and_sets_0600_mode() {
        let (_dir, store) = temp_store("bounded");
        for idx in 0..(MAX_THREAD_TOPICS + 25) {
            let ts = i64::try_from(idx).expect("fits i64") * 100;
            store
                .upsert(ThreadTopic::new(
                    format!("thread-{idx:04}"),
                    "Investors",
                    "decision-model",
                    TopicAuthority::DecisionModel,
                    ts,
                ))
                .expect("upsert");
        }

        let listed = store.list().expect("list");
        assert_eq!(listed.len(), MAX_THREAD_TOPICS);
        assert_eq!(listed[0].thread_id, "thread-0524");
        assert_eq!(listed[MAX_THREAD_TOPICS - 1].thread_id, "thread-0025");
        assert!(store.get("thread-0000").expect("get").is_none());
        assert!(store.get("thread-0024").expect("get").is_none());
        assert!(store.get("thread-0025").expect("get").is_some());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(store.path())
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "store file must be owner-only (0o600)");
        }
    }

    #[test]
    fn correlation_key_and_events_merge_without_altering_person_topic() {
        let (_dir, store) = temp_store("correlation");
        store
            .upsert(
                ThreadTopic::new(
                    "mail-thread-42",
                    "Customers",
                    "person-correction",
                    TopicAuthority::Person,
                    10_000,
                )
                .with_correlation_key("mail:acct-1:msg-99")
                .with_correlated_events(["evt-outage"])
                .with_secondary_topics(["Outage", "Customers"]),
            )
            .expect("initial");

        let updated = store
            .correlate_event(
                "mail-thread-42",
                Some("mail:acct-1:msg-99"),
                "evt-refund",
                10_500,
            )
            .expect("correlate")
            .expect("present");
        assert_eq!(updated.topic, "Customers");
        assert_eq!(updated.authority, TopicAuthority::Person);
        assert_eq!(updated.all_topics(), vec!["Customers", "Outage"]);
        assert_eq!(updated.correlated_events, vec!["evt-outage", "evt-refund"]);

        // Duplicate event id is ignored.
        let deduped = store
            .correlate_event("mail-thread-42", None, "evt-refund", 10_600)
            .expect("correlate dup")
            .expect("present");
        assert_eq!(deduped.correlated_events, vec!["evt-outage", "evt-refund"]);

        let by_key = store
            .by_correlation_key("mail:acct-1:msg-99")
            .expect("by key");
        assert_eq!(by_key.len(), 1);
        assert_eq!(by_key[0].thread_id, "mail-thread-42");
    }
}
