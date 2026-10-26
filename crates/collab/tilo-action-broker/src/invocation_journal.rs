//! Host-private, single-writer invocation admission. Never replicated.
//!
//! The store is one private file, `records.json`: a compacted snapshot, then
//! one appended entry line per durable transition. An admission, a terminal
//! outcome, a cancellation request or an effect preparation each cost one
//! appended entry and one data barrier. Nonterminal progress stays in memory:
//! recovery turns every nonterminal record into an uncertain observation
//! whatever progress it last reported, so persisting progress would buy
//! nothing. Once the appended entries would outgrow the snapshot, the next
//! transition compacts the whole store through an atomic rename instead.
//!
//! No dedup key is evicted. An admission intent's `request` is retained only as
//! a digest, which answers "same request?" exactly as the value did. Any failed
//! write poisons this writer; a restart validates the snapshot, replays every
//! complete entry, ignores only a torn final entry (no caller was ever told of
//! it), turns every nonterminal into an uncertain observation and compacts.
//! Recovery never returns a dispatch permit.
//!
//! A store without entries has exactly the snapshot format earlier writers
//! produced, so their stores open unchanged. An earlier writer refuses a store
//! that still carries entries rather than ignoring them.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
#[path = "invocation_journal/content_puts.rs"]
mod content_puts;
#[path = "invocation_journal/effect.rs"]
mod effect;
#[path = "invocation_journal/guarded.rs"]
mod guarded;
#[cfg(test)]
pub(crate) use content_puts::CONTENT_PUTS_FILE;
pub(crate) use content_puts::ContentPutRecord;
#[path = "invocation_journal/writer.rs"]
mod writer;
pub(crate) use effect::PreparedEffect;
#[cfg(test)]
pub(crate) use writer::Barriers;
pub(crate) use writer::Durable;
use writer::{Counted, Injected, Write as Staged, Writer};
#[cfg(test)]
#[path = "invocation_journal/effect_tests.rs"]
mod effect_tests;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tilo_actions::{InvocationId, InvocationUpdate};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) enum InvocationOwner {
    Local {
        actor: String,
    },
    Certified {
        root: [u8; 32],
        device: [u8; 32],
    },
    /// Exact admitted local-channel identity, distinct from actor-only legacy
    /// ownership and from certified peer callers. This is retained data only.
    AuthenticatedLocal {
        root: [u8; 32],
        device: [u8; 32],
        #[serde(deserialize_with = "required_agent")]
        agent: Option<String>,
    },
    /// Retained ownership data only; observation still requires live task authority.
    Enrolled {
        root: [u8; 32],
        device: [u8; 32],
        task: String,
        run: String,
        channel: String,
    },
    /// Run-scoped replay data. Access still requires the current signed agent
    /// channel and the canonical owner's task admission on every attempt.
    NativeTask {
        agent: String,
        task: String,
        epoch: u64,
    },
}

// Unlike serde's implicit Option default, an omitted field must not silently
// turn a delegated-agent owner into an ordinary person owner.
fn required_agent<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

impl InvocationOwner {
    pub(super) fn verified_actor(&self) -> Option<crate::action_evidence::VerifiedActor> {
        use crate::action_evidence::VerifiedActor as Actor;
        Some(match self {
            Self::Local { .. } => return None,
            Self::Certified { root, device } => Actor::Certified {
                root: *root,
                device: *device,
            },
            Self::AuthenticatedLocal {
                root,
                device,
                agent,
            } => Actor::AuthenticatedLocal {
                root: *root,
                device: *device,
                agent: agent.clone(),
            },
            Self::Enrolled {
                root,
                device,
                task,
                run,
                channel,
            } => Actor::Enrolled {
                root: *root,
                device: *device,
                task: task.clone(),
                run: run.clone(),
                channel: channel.clone(),
            },
            Self::NativeTask { agent, task, epoch } => Actor::NativeTask {
                // Filled from original LocalCaller custody before signing.
                root: [0; 32],
                device: [0; 32],
                agent: agent.clone(),
                task: task.clone(),
                epoch: *epoch,
            },
        })
    }
    pub(super) fn validate(&self) -> Result<(), String> {
        if let Self::NativeTask { agent, task, epoch } = self
            && (agent.is_empty()
                || agent.len() > 512
                || agent.chars().any(char::is_control)
                || task.is_empty()
                || task.len() > 256
                // An issue's run, or a room task's in a journal kept from
                // before tasks were issues.
                || !(task.starts_with("task:") || crate::services::is_tracker_issue(task))
                || *epoch == 0)
        {
            return Err("invalid native task invocation owner".into());
        }
        if let Self::AuthenticatedLocal {
            agent: Some(agent), ..
        } = self
            && (agent.is_empty()
                || agent.len() > 512
                || agent.bytes().any(|byte| byte.is_ascii_control()))
        {
            return Err("invalid authenticated local invocation agent".into());
        }
        if let Self::Enrolled {
            task, run, channel, ..
        } = self
        {
            for value in [task, run, channel] {
                let id = uuid::Uuid::parse_str(value)
                    .map_err(|_| "invalid enrolled invocation owner")?;
                if value.len() != 36 || id.is_nil() || id.to_string() != *value {
                    return Err("noncanonical enrolled invocation owner".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InvocationKey {
    pub room: String,
    pub host: uuid::Uuid,
    pub owner: InvocationOwner,
    pub invocation: InvocationId,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DurableInvocation {
    pub(super) key: InvocationKey,
    /// The admission intent. Its `request`, when it had one, is retained only
    /// as `request_digest`: ask [`Self::admitted_request_is`], never this map.
    pub(super) intent: serde_json::Value,
    request_digest: Option<guarded::Digest>,
    pub wire_id: u64,
    pub update: InvocationUpdate,
    pub uncertain: bool,
    pub cancellation_requested: bool,
    guarded: Option<guarded::State>,
    pub(crate) prepared_effect: Option<PreparedEffect>,
    pub(crate) admission: Option<crate::action_evidence::AdmissionEvidence>,
    pub(crate) terminal: Option<crate::action_evidence::TerminalReceipt>,
}

impl DurableInvocation {
    /// Whether `request` is the request this invocation was admitted with.
    /// A record without one compares as `null`, like an absent JSON member.
    pub(crate) fn admitted_request_is(&self, request: &serde_json::Value) -> bool {
        match (&self.request_digest, self.intent.get("request")) {
            (Some(digest), _) => *digest == request_digest(request),
            (None, Some(retained)) => retained == request,
            (None, None) => request.is_null(),
        }
    }

    /// Whether `intent` is the whole intent this invocation was admitted with.
    fn admitted_intent_is(&self, intent: &serde_json::Value) -> bool {
        if self.request_digest.is_none() {
            return self.intent == *intent;
        }
        let (rest, digest) = retained_intent(intent.clone());
        digest == self.request_digest && rest == self.intent
    }
}

/// Splits an admission intent into what is retained verbatim and the digest
/// of its `request`, the one member that carries the caller's arguments.
fn retained_intent(mut intent: serde_json::Value) -> (serde_json::Value, Option<guarded::Digest>) {
    let request = intent
        .as_object_mut()
        .and_then(|fields| fields.remove("request"));
    (intent, request.as_ref().map(request_digest))
}

/// The journal's exact retry digest, also bound by private admission evidence.
pub(crate) fn admission_request_digest(request: &serde_json::Value) -> [u8; 32] {
    request_digest(request).0
}

/// A digest of `request` that is equal exactly when the JSON values are:
/// object members are hashed in key order, whatever order they arrived in.
fn request_digest(request: &serde_json::Value) -> guarded::Digest {
    fn canonical(value: &serde_json::Value, out: &mut Vec<u8>) {
        match value {
            serde_json::Value::Object(fields) => {
                let mut members: Vec<_> = fields.iter().collect();
                members.sort_unstable_by(|left, right| left.0.cmp(right.0));
                out.push(b'{');
                for (position, (name, member)) in members.into_iter().enumerate() {
                    if position > 0 {
                        out.push(b',');
                    }
                    canonical(&serde_json::Value::String(name.clone()), out);
                    out.push(b':');
                    canonical(member, out);
                }
                out.push(b'}');
            }
            serde_json::Value::Array(items) => {
                out.push(b'[');
                for (position, item) in items.iter().enumerate() {
                    if position > 0 {
                        out.push(b',');
                    }
                    canonical(item, out);
                }
                out.push(b']');
            }
            scalar => out.extend_from_slice(scalar.to_string().as_bytes()),
        }
    }
    let mut encoded = Vec::new();
    canonical(request, &mut encoded);
    let mut hasher = blake3::Hasher::new_derive_key("tilo invocation journal request v1");
    hasher.update(&encoded);
    guarded::Digest(*hasher.finalize().as_bytes())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    host: uuid::Uuid,
    records: Vec<DurableInvocation>,
}

/// One durable transition appended after the snapshot: the record's whole
/// row as it now stands, and the writer floor it was written under.
#[derive(Serialize)]
struct EntryRef<'a> {
    version: u32,
    record: &'a DurableInvocation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    version: u32,
    record: DurableInvocation,
}

/// Where each record sits, by the two identities that are unique store-wide.
#[derive(Default)]
struct Index {
    by_invocation: HashMap<InvocationId, usize>,
    wire_ids: HashSet<u64>,
}

impl Index {
    fn insert(
        &mut self,
        records: &mut Vec<DurableInvocation>,
        record: DurableInvocation,
    ) -> Result<usize, String> {
        if self.by_invocation.contains_key(&record.key.invocation)
            || !self.wire_ids.insert(record.wire_id)
        {
            return Err("invocation journal contains inconsistent identities".into());
        }
        let at = records.len();
        self.by_invocation.insert(record.key.invocation, at);
        records.push(record);
        Ok(at)
    }
}

/// The store the writer appends to, as the journal staged it.
struct Tail {
    /// Staged length: the snapshot plus every staged entry.
    len: u64,
    /// Length of the snapshot this file was compacted to.
    snapshot: u64,
}

impl Tail {
    /// Whether one more entry of `bytes` stays an append rather than a
    /// compaction: entries never outgrow their snapshot (or the floor below
    /// which compacting is not worth its rename) and never the size limit.
    fn takes(&self, bytes: u64) -> bool {
        let entries = self.len.saturating_sub(self.snapshot).saturating_add(bytes);
        entries <= self.snapshot.max(InvocationJournal::COMPACT_AFTER)
            && self.len.saturating_add(bytes) <= InvocationJournal::MAX_BYTES
    }
}

/// A store file decoded up to recovery.
struct Loaded {
    snapshot: Snapshot,
    index: Index,
    /// Complete entries replayed after the snapshot.
    entries: usize,
    /// A final entry was torn by a crash and ignored.
    torn: bool,
    /// The file ends exactly where an entry may be appended.
    appendable: bool,
    len: u64,
}

impl Loaded {
    fn empty(host: uuid::Uuid) -> Self {
        Self {
            snapshot: Snapshot {
                version: 1,
                host,
                records: Vec::new(),
            },
            index: Index::default(),
            entries: 0,
            torn: false,
            appendable: false,
            len: 0,
        }
    }

    fn read(file: &mut File) -> Result<Self, String> {
        let len = file
            .metadata()
            .map_err(|_| "invocation journal metadata unreadable")?
            .len();
        if len > InvocationJournal::MAX_BYTES {
            return Err("invocation journal exceeds storage limit".into());
        }
        let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or_default());
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "invocation journal rewind failed")?;
        Read::by_ref(file)
            .take(InvocationJournal::MAX_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|_| "invocation journal snapshot missing or unreadable")?;
        let mut values =
            serde_json::Deserializer::from_slice(&bytes).into_iter::<serde::de::IgnoredAny>();
        if !matches!(values.next(), Some(Ok(_))) {
            return Err("invocation journal snapshot is corrupt or unsupported".into());
        }
        let (head, rest) = bytes
            .split_at_checked(values.byte_offset())
            .ok_or("invocation journal snapshot is corrupt or unsupported")?;
        guarded::preflight(&mut &*head)?;
        let snapshot: Snapshot = serde_json::from_slice(head)
            .map_err(|_| "invocation journal snapshot is corrupt or unsupported")?;
        if !matches!(snapshot.version, 1..=7) || snapshot.host.is_nil() {
            return Err("invocation journal version or host unsupported".into());
        }
        let mut loaded = Self {
            snapshot: Snapshot {
                version: snapshot.version,
                host: snapshot.host,
                records: Vec::with_capacity(snapshot.records.len()),
            },
            index: Index::default(),
            entries: 0,
            torn: false,
            appendable: rest == b"\n",
            len: bytes.len() as u64,
        };
        for record in snapshot.records {
            check_record(&record, loaded.snapshot.version, loaded.snapshot.host)?;
            loaded.index.insert(&mut loaded.snapshot.records, record)?;
        }
        if rest.iter().all(u8::is_ascii_whitespace) {
            return Ok(loaded);
        }
        let entries = rest
            .strip_prefix(b"\n")
            .ok_or("invocation journal trailing bytes")?;
        let mut lines = entries.split_inclusive(|byte| *byte == b'\n').peekable();
        while let Some(line) = lines.next() {
            let Some(line) = line.strip_suffix(b"\n").filter(|line| !line.contains(&0)) else {
                // A crash mid-append leaves the final entry unterminated or
                // with unwritten (zero) blocks. Its barrier never returned,
                // so no caller was told of it. Only the final one may be torn.
                if lines.peek().is_some() {
                    return Err("invocation journal entry is corrupt".into());
                }
                loaded.torn = true;
                break;
            };
            guarded::preflight(&mut &*line)?;
            let entry: Entry = serde_json::from_slice(line)
                .map_err(|_| "invocation journal entry is corrupt or unsupported")?;
            loaded.replay(entry)?;
        }
        Ok(loaded)
    }

    fn replay(&mut self, entry: Entry) -> Result<(), String> {
        if !matches!(entry.version, 1..=7) {
            return Err("invocation journal entry version unsupported".into());
        }
        // The floor only rises. Earlier writers stamped every entry after an
        // effect preparation 5, even above a native task's floor of 6; such an
        // entry is replayed under the raised floor, which every row must still
        // satisfy, so a store they left behind reopens instead of refusing.
        let floor = self.snapshot.version.max(entry.version);
        check_record(&entry.record, floor, self.snapshot.host)?;
        self.snapshot.version = floor;
        self.entries = self.entries.saturating_add(1);
        let Some(&at) = self.index.by_invocation.get(&entry.record.key.invocation) else {
            return self
                .index
                .insert(&mut self.snapshot.records, entry.record)
                .map(drop);
        };
        let retained = self
            .snapshot
            .records
            .get_mut(at)
            .ok_or("invocation journal index inconsistent")?;
        // Entries only move a live record forward; a retired one is final.
        if retained.key != entry.record.key
            || retained.wire_id != entry.record.wire_id
            || retained.update.is_terminal()
        {
            return Err("invocation journal entry rewrites a retired or foreign record".into());
        }
        *retained = entry.record;
        Ok(())
    }
}

/// Every check one stored row must pass under the writer floor `version`.
pub(super) fn evidence_facts(
    key: &InvocationKey,
    intent: &serde_json::Value,
) -> Result<crate::action_evidence::AdmissionFacts, String> {
    let text = |field| {
        intent
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("admission {field} missing"))
    };
    Ok(crate::action_evidence::AdmissionFacts {
        host: key.host,
        room: key.room.clone(),
        invocation: key.invocation,
        actor: key
            .owner
            .verified_actor()
            .ok_or("legacy actor cannot attest a user")?,
        actor_id: text("actor")?,
        app_id: text("app_id")?,
        record_id: text("record_id")?,
        resource: text("resource")?,
        action: text("action")?,
        revision: intent
            .get("revision")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

fn check_evidence(
    key: &InvocationKey,
    intent: &serde_json::Value,
    evidence: &crate::action_evidence::AdmissionEvidence,
) -> Result<(), String> {
    let mut expected = evidence_facts(key, intent)?;
    if let (
        crate::action_evidence::VerifiedActor::NativeTask { root, device, .. },
        crate::action_evidence::VerifiedActor::NativeTask {
            root: held_root,
            device: held_device,
            ..
        },
    ) = (&mut expected.actor, &evidence.receipt.facts.actor)
    {
        if *held_root == [0; 32] || *held_device == [0; 32] {
            return Err("task admission lacks original caller identity".into());
        }
        *root = *held_root;
        *device = *held_device;
    }
    evidence.validate(&expected)?;
    evidence.validate_retained(intent)
}

fn check_record(record: &DurableInvocation, version: u32, host: uuid::Uuid) -> Result<(), String> {
    record.key.owner.validate()?;
    if let Some(evidence) = &record.admission {
        if version < 7 {
            return Err("signed admission requires journal version 7".into());
        }
        check_evidence(&record.key, &record.intent, evidence)?;
        evidence.validate_request_digest(
            record
                .request_digest
                .as_ref()
                .map(|digest| digest.0)
                .or_else(|| record.intent.get("request").map(admission_request_digest)),
        )?;
    }
    if let Some(terminal) = &record.terminal {
        let admission = record
            .admission
            .as_ref()
            .ok_or("terminal lacks admission")?;
        crate::action_evidence::verify_terminal(terminal, admission.public_receipt())?;
        if Some(terminal.category)
            != crate::action_evidence::terminal_category(&record.update, record.uncertain)
        {
            return Err("terminal category differs from retained outcome".into());
        }
    }
    if version < 6 && matches!(record.key.owner, InvocationOwner::NativeTask { .. }) {
        return Err("native task replay ownership requires journal version 6".into());
    }
    if let Some(prepared) = &record.prepared_effect {
        if version < 5 {
            return Err("prepared effect requires journal version 5".into());
        }
        prepared.validate(record)?;
    }
    if record.guarded.is_some() && version < 3 {
        return Err("guarded read requires journal version 3".into());
    }
    if version < 4 && matches!(record.key.owner, InvocationOwner::AuthenticatedLocal { .. }) {
        return Err("authenticated local ownership requires journal version 4".into());
    }
    if version == 1 && matches!(record.key.owner, InvocationOwner::Enrolled { .. }) {
        return Err("private invocation ownership requires journal version 2".into());
    }
    if record.request_digest.is_some()
        && record
            .intent
            .as_object()
            .is_none_or(|fields| fields.contains_key("request"))
    {
        return Err("invocation journal retains a request beside its digest".into());
    }
    if record.key.host != host
        || record.key.invocation != record.update.invocation()
        || record.wire_id == 0
    {
        return Err("invocation journal contains inconsistent identities".into());
    }
    Ok(())
}

pub(super) struct InvocationJournal {
    root: PathBuf,
    snapshot: Snapshot,
    index: Index,
    /// The store the writer appends to, as staged; `None` makes the next
    /// transition compact (an earlier writer's snapshot ends without an
    /// entry separator).
    tail: Option<Tail>,
    /// Makes staged transitions durable, in order, off the caller's thread;
    /// `None` only while the journal opens.
    writer: Option<Writer>,
    /// The store opened and its lock, until the writer takes them over.
    opening: Option<(Option<File>, advisory_lock::FileLock)>,
    counted: Counted,
    /// What a reader may see of each invocation with a write in flight: as
    /// it was before the first, until each write is durable.
    undurable: HashMap<InvocationId, Undurable>,
    /// The record [`Self::record_mut`] last handed out, as it was before.
    before: Option<DurableInvocation>,
    poisoned: bool,
    #[cfg(test)]
    pub fail_at: Option<BarrierFailure>,
    /// One-shot: the next write is made durable but reported as failed.
    #[cfg(test)]
    pub unconfirmed_next: bool,
    /// One-shot pause after durable admission, while the writer is still held.
    #[cfg(test)]
    pub after_admission: Option<Box<dyn FnOnce() + Send>>,
    /// Run before each write starts, on the thread making it, as a slow
    /// disk would hold it.
    #[cfg(test)]
    pub hold_writes: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

/// An invocation's writes in flight: readers see `durable` until a write
/// resolves, then the record that write made durable.
struct Undurable {
    durable: Option<DurableInvocation>,
    writes: Vec<(DurableInvocation, std::sync::Arc<writer::Ticket>)>,
}

impl Undurable {
    /// The record as durable now, or `None` once no write is in flight.
    fn visible(&self) -> Option<&DurableInvocation> {
        self.writes
            .iter()
            .rev()
            .find(|(_, ticket)| matches!(ticket.outcome(), Some(Ok(()))))
            .map(|(record, _)| record)
            .or(self.durable.as_ref())
    }
    fn settled(&self) -> bool {
        self.writes
            .iter()
            .all(|(_, ticket)| ticket.outcome().is_some())
    }
    fn last(&self) -> Durable {
        self.writes
            .last()
            .map_or_else(Durable::settled, |(_, ticket)| Durable::of(ticket.clone()))
    }
}

/// An injected failure. `File` fails before the write is durable, so the
/// transition is absent after restart; `Directory` fails once the write is
/// durable (after an entry's data barrier, or a compaction's rename).
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BarrierFailure {
    File,
    Directory,
    AfterAdmission,
    Terminal,
}

impl InvocationJournal {
    const MAX_BYTES: u64 = 256 * 1024 * 1024;
    /// Entries may always grow to this before a compaction is due.
    const COMPACT_AFTER: u64 = 1024 * 1024;

    pub fn open(root: &Path) -> Result<Self, String> {
        Self::open_with_parent_barrier(root, |parent| {
            File::open(parent).and_then(|file| file.sync_all())
        })
    }

    fn open_with_parent_barrier(
        root: &Path,
        parent_barrier: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<Self, String> {
        Self::open_with_barriers(root, parent_barrier, |file, directory| {
            file.sync_all()?;
            directory.sync_all()
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered open, lock, decode and recovery pass over the journal store"
    )]
    fn open_with_barriers(
        root: &Path,
        parent_barrier: impl FnOnce(&Path) -> std::io::Result<()>,
        guarded_barrier: impl FnOnce(&File, &File) -> std::io::Result<()>,
    ) -> Result<Self, String> {
        let created = match std::fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(_) => return Err("invocation journal directory unavailable".into()),
        };
        let metadata = std::fs::symlink_metadata(root)
            .map_err(|_| "invocation journal directory unreadable")?;
        if !metadata.is_dir() || metadata.permissions().mode() & 0o777 != 0o700 {
            return Err("invocation journal requires a private directory".into());
        }
        // Retain the directory actually opened for this snapshot's guarded
        // reconciliation, rather than reopening its pathname after decoding.
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(root)
            .map_err(|_| "invocation journal directory unavailable")?;
        let actual_directory = directory
            .metadata()
            .map_err(|_| "invocation journal directory metadata unavailable")?;
        if actual_directory.dev() != metadata.dev() || actual_directory.ino() != metadata.ino() {
            return Err("invocation journal directory changed during open".into());
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join("owner.lock"))
            .map_err(|_| "invocation journal lock unavailable")?;
        private_file(&lock)?;
        let lock = advisory_lock::FileLock::try_exclusive(lock)
            .map_err(|_| "invocation journal already has a writer")?;
        let mut observed_store = None;
        // The first snapshot is written (by rename) only after the directory
        // and its lock exist, so a store whose first open was cut short holds
        // no snapshot at all: it was never initialized, and initializing it
        // now loses nothing. Every initialized store keeps one, replaced
        // atomically, so any other failure to open it is refused.
        let opened = if created {
            None
        } else {
            // Opened for appends too: a clean store keeps exactly this file.
            match OpenOptions::new()
                .read(true)
                .append(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(root.join("records.json"))
            {
                Ok(file) => Some(file),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err("invocation journal snapshot missing or unreadable".into()),
            }
        };
        let created = opened.is_none();
        let loaded = match opened {
            None => Loaded::empty(uuid::Uuid::new_v4()),
            Some(mut file) => {
                private_file(&file)?;
                let loaded = Loaded::read(&mut file)?;
                observed_store = Some(file);
                loaded
            }
        };
        let mut journal = Self {
            root: root.to_owned(),
            snapshot: loaded.snapshot,
            index: loaded.index,
            tail: None,
            writer: None,
            opening: Some((None, lock)),
            counted: Counted::default(),
            undurable: HashMap::new(),
            before: None,
            poisoned: false,
            #[cfg(test)]
            fail_at: None,
            #[cfg(test)]
            unconfirmed_next: false,
            #[cfg(test)]
            after_admission: None,
            #[cfg(test)]
            hold_writes: None,
        };
        let mut recovered = false;
        for record in &mut journal.snapshot.records {
            if !record.update.is_terminal() {
                record.update = if record.prepared_effect.is_some() {
                    InvocationUpdate::Accepted {
                        invocation: record.key.invocation,
                    }
                } else {
                    unknown(record.key.invocation)
                };
                record.uncertain = true;
                recovered = true;
            }
        }
        if created || recovered || loaded.entries > 0 || loaded.torn {
            // Fold the replayed entries (dropping a torn one) and the recovered
            // records into one fresh snapshot before anything is exposed.
            journal.compact()?;
        } else {
            if loaded.appendable
                || journal
                    .snapshot
                    .records
                    .iter()
                    .any(|record| record.guarded.is_some() || record.prepared_effect.is_some())
            {
                // The decoded file may be the result of a rename whose directory
                // barrier failed. Reconcile the exact decoded file, not a freshly
                // reopened name, before exposing even sanitized guarded status or
                // appending entries a lost rename would orphan.
                guarded_barrier(
                    observed_store
                        .as_ref()
                        .ok_or("guarded snapshot unavailable")?,
                    &directory,
                )
                .map_err(|_| "invocation journal snapshot reconciliation failed")?;
            }
            let appended = observed_store.filter(|_| loaded.appendable);
            journal.tail = appended.as_ref().map(|_| Tail {
                len: loaded.len,
                snapshot: loaded.len,
            });
            if let Some((tail, _)) = journal.opening.as_mut() {
                *tail = appended;
            }
        }
        // A failed initialization may leave a valid snapshot. Its existence
        // does not prove the directory entry was durable: retry on every open.
        parent_barrier(root.parent().ok_or("invocation journal needs a parent")?)
            .map_err(|_| "invocation journal parent barrier failed")?;
        let (tail, lock) = journal
            .opening
            .take()
            .ok_or("invocation journal opened twice")?;
        journal.writer = Some(Writer::start(
            journal.root.clone(),
            tail,
            lock,
            journal.counted.clone(),
        )?);
        Ok(journal)
    }

    pub fn host(&self) -> uuid::Uuid {
        self.snapshot.host
    }
    pub fn next_wire_id(&self) -> u64 {
        self.snapshot
            .records
            .iter()
            .map(|r| r.wire_id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    /// The position of exactly `key`; its UUID alone is unique store-wide.
    fn position(&self, key: &InvocationKey) -> Option<usize> {
        let at = *self.index.by_invocation.get(&key.invocation)?;
        self.snapshot
            .records
            .get(at)
            .is_some_and(|record| record.key == *key)
            .then_some(at)
    }

    fn record_mut(
        &mut self,
        key: &InvocationKey,
    ) -> Result<(usize, &mut DurableInvocation), &'static str> {
        let at = self
            .position(key)
            .ok_or("unknown invocation journal identity")?;
        let record = self
            .snapshot
            .records
            .get_mut(at)
            .ok_or("unknown invocation journal identity")?;
        self.before = Some(record.clone());
        Ok((at, record))
    }

    /// The write a transition of `invocation` must wait for: the last one in
    /// flight, if any.
    pub(crate) fn in_flight(&self, invocation: &InvocationId) -> Durable {
        self.undurable
            .get(invocation)
            .map_or_else(Durable::settled, Undurable::last)
    }

    /// The record as durable now: a transition still being written is not
    /// visible until it is.
    pub fn lookup(&self, key: &InvocationKey) -> Result<Option<DurableInvocation>, String> {
        self.check()?;
        let Some(at) = self.position(key) else {
            return Ok(None);
        };
        if let Some(undurable) = self
            .undurable
            .get(&key.invocation)
            .filter(|undurable| !undurable.settled())
        {
            return Ok(undurable.visible().cloned());
        }
        Ok(self.snapshot.records.get(at).cloned())
    }

    /// Journal fixtures may exercise admission without signing identities.
    #[cfg(test)]
    pub fn admit(
        &mut self,
        key: InvocationKey,
        intent: serde_json::Value,
        wire_id: u64,
    ) -> Result<bool, String> {
        let (admitted, durable) = self.admit_with_evidence(key, intent, wire_id, None)?;
        durable.wait_blocking()?;
        Ok(admitted)
    }

    /// The evidence and admission share the original append and data barrier.
    /// Whether this admitted the invocation, and the write to wait for before
    /// it is delivered or acknowledged: this admission's, or the one still in
    /// flight of an invocation already admitted.
    pub(super) fn admit_with_evidence(
        &mut self,
        key: InvocationKey,
        intent: serde_json::Value,
        wire_id: u64,
        admission: Option<crate::action_evidence::AdmissionEvidence>,
    ) -> Result<(bool, Durable), String> {
        self.check()?;
        if let Some(evidence) = &admission {
            check_evidence(&key, &intent, evidence)?;
            evidence
                .validate_request_digest(intent.get("request").map(admission_request_digest))?;
            evidence
                .opening_for_owner(&evidence.receipt.facts.actor)?
                .verify(&evidence.receipt, &intent)?;
            self.snapshot.version = self.snapshot.version.max(7);
        }
        key.owner.validate()?;
        if let Some(existing) = self.position(&key) {
            if !self
                .snapshot
                .records
                .get(existing)
                .is_some_and(|record| record.admitted_intent_is(&intent))
            {
                return Err("invocation identity reused with different intent".into());
            }
            return Ok((false, self.in_flight(&key.invocation)));
        }
        // Application lifecycle routing and internal dedup use the full UUID
        // without the owner. Permanently reserve it for one durable key so a
        // different device, room, or legacy caller can never alias that state.
        if self.index.by_invocation.contains_key(&key.invocation) {
            return Err("action_invocation_identity_conflict: UUID already reserved".into());
        }
        if key.host != self.host() || wire_id == 0 || self.index.wire_ids.contains(&wire_id) {
            return Err("invocation journal identity conflict".into());
        }
        let update = InvocationUpdate::Accepted {
            invocation: key.invocation,
        };
        if matches!(key.owner, InvocationOwner::Enrolled { .. }) {
            self.snapshot.version = self.snapshot.version.max(2);
        }
        if matches!(key.owner, InvocationOwner::AuthenticatedLocal { .. }) {
            self.snapshot.version = self.snapshot.version.max(4);
        }
        if matches!(key.owner, InvocationOwner::NativeTask { .. }) {
            self.snapshot.version = self.snapshot.version.max(6);
        }
        let (intent, request_digest) = retained_intent(intent);
        let at = self.index.insert(
            &mut self.snapshot.records,
            DurableInvocation {
                key,
                intent,
                request_digest,
                wire_id,
                update,
                uncertain: false,
                cancellation_requested: false,
                guarded: None,
                prepared_effect: None,
                admission,
                terminal: None,
            },
        )?;
        self.before = None;
        #[cfg_attr(
            not(test),
            expect(unused_mut, reason = "injections are made only under test")
        )]
        let mut injected = Injected::default();
        #[cfg(test)]
        {
            injected.after = self.after_admission.take();
            if self.fail_at == Some(BarrierFailure::AfterAdmission) {
                self.fail_at = None;
                injected.crash_after = true;
            }
        }
        let durable = self.commit(at, injected)?;
        Ok((true, durable))
    }

    /// [`Self::record_update`], waiting for the write here.
    #[cfg(test)]
    pub fn update(
        &mut self,
        key: &InvocationKey,
        update: InvocationUpdate,
        uncertain: bool,
    ) -> Result<(), String> {
        let at = self
            .position(key)
            .ok_or("unknown invocation journal identity")?;
        let started = self.snapshot.records.get(at).cloned();
        let write = self.record_update(key, update, uncertain)?;
        let started = started.ok_or("unknown invocation journal identity")?;
        self.settle(at, started, write)
    }

    /// A terminal outcome is durable once the returned write resolves.
    /// Nonterminal progress is kept for observers only: recovery discards it
    /// anyway, so it is visible at once and nothing is written.
    pub fn record_update(
        &mut self,
        key: &InvocationKey,
        update: InvocationUpdate,
        uncertain: bool,
    ) -> Result<Durable, String> {
        self.check()?;
        let (at, record) = self.record_mut(key)?;
        if record.update.is_terminal() || update.invocation() != key.invocation {
            return Err("invocation journal transition rejected".into());
        }
        if record.guarded.is_some() && !guarded::allowed_failure(&update, uncertain) {
            return Err(
                "guarded invocation requires a sanitized failure or private terminal".into(),
            );
        }
        if let Some(prepared) = &record.prepared_effect
            && (update != prepared.update || uncertain)
        {
            return Err("prepared effect requires its exact owner-accepted terminal".into());
        }
        let terminal = update.is_terminal();
        record.update = update;
        record.uncertain = uncertain;
        if terminal {
            let settled = self
                .undurable
                .get(&key.invocation)
                .is_none_or(Undurable::settled);
            let durable = self.commit(at, Injected::default())?;
            // An uncertain outcome claims only what recovery would answer
            // anyway, so it is seen at once over a durable record.
            if uncertain
                && settled
                && let Some(undurable) = self.undurable.get_mut(&key.invocation)
            {
                undurable.durable = self.snapshot.records.get(at).cloned();
            }
            Ok(durable)
        } else {
            self.before = None;
            Ok(self.in_flight(&key.invocation))
        }
    }

    // Storage prerequisites only. No production caller is wired until the
    // genuine selected worker/owner capsule protocol is installed.
    #[cfg(test)]
    fn require_guarded_read(
        &mut self,
        key: &InvocationKey,
        selection: [u8; 32],
    ) -> Result<(), String> {
        self.check()?;
        let (at, record) = self.record_mut(key)?;
        if let Some(guarded) = &record.guarded {
            return if guarded.selection() == &selection {
                Ok(())
            } else {
                Err("changed guarded selection".into())
            };
        }
        if !matches!(record.update, InvocationUpdate::Accepted { .. })
            || record.uncertain
            || record.cancellation_requested
        {
            return Err("guarded selection requires fresh accepted invocation".into());
        }
        let started = record.clone();
        record.guarded = Some(guarded::State::selected(selection));
        self.snapshot.version = self.snapshot.version.max(3);
        let write = self.commit(at, Injected::default())?;
        self.settle(at, started, write)
    }

    #[cfg(test)]
    fn complete_guarded_read(
        &mut self,
        key: &InvocationKey,
        selection: [u8; 32],
        output: [u8; 32],
        basis: [u8; 32],
        bytes: u32,
    ) -> Result<(), String> {
        self.check()?;
        let (at, record) = self.record_mut(key)?;
        if !matches!(record.update, InvocationUpdate::Accepted { .. })
            || record.uncertain
            || record.cancellation_requested
        {
            return Err("guarded terminal requires live accepted invocation".into());
        }
        let guarded = record.guarded.as_mut().ok_or("guarded selection absent")?;
        if guarded.selection() != &selection {
            return Err("changed guarded selection".into());
        }
        guarded.complete(key.invocation, output, basis, bytes)?;
        let started = record.clone();
        record.update = guarded::unavailable(key.invocation);
        record.uncertain = false;
        let write = self.commit(at, Injected::default())?;
        self.settle(at, started, write)
    }

    /// [`Self::record_cancellation`], waiting for the write here.
    #[cfg(test)]
    pub fn cancel(&mut self, key: &InvocationKey) -> Result<(), String> {
        let at = self
            .position(key)
            .ok_or("unknown invocation journal identity")?;
        let started = self.snapshot.records.get(at).cloned();
        let write = self.record_cancellation(key)?;
        let started = started.ok_or("unknown invocation journal identity")?;
        self.settle(at, started, write)
    }

    /// The cancellation request is durable once the returned write resolves.
    pub fn record_cancellation(&mut self, key: &InvocationKey) -> Result<Durable, String> {
        self.check()?;
        let (at, record) = self.record_mut(key)?;
        if record.update.is_terminal() {
            return Err("invocation is no longer running".into());
        }
        if record.guarded.is_some() {
            return Err("guarded reads are noncancellable".into());
        }
        record.cancellation_requested = true;
        self.commit(at, Injected::default())
    }

    /// Whether a transition may be staged.
    ///
    /// A write that failed with none of its bytes durable leaves this journal
    /// as the last durable write did, so that failure poisoned nothing: the
    /// writer simply attempts the next write. A write that may be durable
    /// although its caller was told it failed leaves the store holding what
    /// this writer cannot account for; [`Self::recover`] reads it back.
    pub(super) fn check(&self) -> Result<(), String> {
        if self.failed() {
            Err("invocation journal requires recovery after a failed barrier".into())
        } else {
            Ok(())
        }
    }

    /// Whether this journal requires recovery.
    pub fn failed(&self) -> bool {
        self.poisoned || self.writer.as_ref().is_some_and(Writer::failed)
    }

    /// Recovery without a restart, for a journal whose store may hold a write
    /// no caller was told of. The writer that still holds the store's lock
    /// goes on with it, and every admission, key and wire id already made
    /// stays known, so nothing is reissued and nothing is lost. A transition
    /// after it fails again if the disk still cannot keep it.
    pub fn recover(&mut self) -> Result<(), String> {
        if self.poisoned {
            return Err("invocation journal requires recovery after a failed barrier".into());
        }
        let Some(writer) = self.writer.as_mut() else {
            return Err("invocation journal requires recovery after a failed barrier".into());
        };
        writer.recovered();
        self.reopen()
    }

    /// Folds the store as it is on disk into one snapshot, turning every
    /// nonterminal invocation uncertain, exactly as opening it does.
    fn reopen(&mut self) -> Result<(), String> {
        if self.opening.is_some() {
            return Err("invocation journal requires recovery after a failed barrier".into());
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join("records.json"))
            .map_err(|_| "invocation journal snapshot missing or unreadable")?;
        private_file(&file)?;
        let loaded = Loaded::read(&mut file)?;
        self.snapshot = loaded.snapshot;
        self.index = loaded.index;
        self.tail = None;
        self.undurable.clear();
        self.before = None;
        for record in &mut self.snapshot.records {
            if !record.update.is_terminal() {
                record.update = if record.prepared_effect.is_some() {
                    InvocationUpdate::Accepted {
                        invocation: record.key.invocation,
                    }
                } else {
                    unknown(record.key.invocation)
                };
                record.uncertain = true;
            }
        }
        // Compaction rewrites the store through the atomic rename a reader
        // only ever sees whole; waiting for its write waits there for the
        // writer that holds the store's lock, so recovery completes before
        // this returns.
        self.compact()
    }

    /// Stages record `at` as it now stands for the writer: one appended
    /// entry, or a compaction once the entries would outgrow the snapshot.
    /// Readers keep seeing the record as it was until the write is durable.
    fn commit(&mut self, at: usize, injected: Injected) -> Result<Durable, String> {
        let before = self.before.take();
        if let Some(record) = self.snapshot.records.get_mut(at)
            && let Some(admission) = &record.admission
            && record.terminal.is_none()
            && record.guarded.is_none()
            && let Some(category) =
                crate::action_evidence::terminal_category(&record.update, record.uncertain)
        {
            // Losing signing identity must not lose an application's outcome.
            // No receipt is preferable to falsely certifying a completion;
            // recovery uncertainty is never retroactively signed as success.
            record.terminal =
                crate::action_evidence::mint_terminal(admission.public_receipt(), category).ok();
        }
        let record = self
            .snapshot
            .records
            .get(at)
            .ok_or("unknown invocation journal identity")?
            .clone();
        // `record_mut` in a later stage records the record as it is now; the
        // one this transition started from is the caller's `before`.
        let started = before.unwrap_or_else(|| record.clone());
        let staged = self
            .encode_entry(&record)
            .and_then(|write| self.stage(write, injected));
        let durable = match staged {
            Ok(durable) => durable,
            Err(error) => {
                self.poison();
                return Err(error);
            }
        };
        // `record` must read as the store has it once the transition settles:
        // a write that left nothing durable is undone, never one that is
        // durable. The writer resolves a write before the callers that wait
        // for it see the outcome, so a write still in flight (under test,
        // held) is undone where it is waited for, by `settle`.
        let ticket = durable.ticket();
        let outcome = ticket.as_ref().and_then(|ticket| ticket.outcome());
        let withdrawn = outcome.as_ref().is_some_and(writer::withdrew);
        if withdrawn {
            self.roll_back(at, started.clone());
        }
        self.undurable.retain(|_, undurable| !undurable.settled());
        // What readers see is what is durable, never a write still in flight
        // or one that made nothing durable.
        if let Some(ticket) =
            ticket.filter(|_| !withdrawn && matches!(outcome, None | Some(Ok(()))))
        {
            self.undurable
                .entry(record.key.invocation)
                .or_insert_with(|| Undurable {
                    durable: Some(started.clone()),
                    writes: Vec::new(),
                })
                .writes
                .push((record, ticket.clone()));
        }
        Ok(durable)
    }

    /// Rewrites the whole store as one snapshot through an atomic rename.
    fn compact(&mut self) -> Result<(), String> {
        let outcome = self
            .encode_snapshot()
            .and_then(|write| self.stage(write, Injected::default()));
        match outcome {
            Ok(durable) => durable.wait_blocking(),
            Err(error) => {
                self.poison();
                Err(error)
            }
        }
    }

    fn poison(&mut self) {
        self.poisoned = true;
        self.tail = None;
    }

    /// Puts record `at` back as `started`, as the store still has it: a
    /// write that left nothing durable must not be read as durable.
    fn roll_back(&mut self, at: usize, started: DurableInvocation) {
        if let Some(record) = self.snapshot.records.get_mut(at) {
            record.clone_from(&started);
        }
    }

    /// `write` as durable here: the outcome, once it is waited for, with the
    /// record it made put back if the write left nothing durable.
    fn settle(
        &mut self,
        at: usize,
        started: DurableInvocation,
        write: Durable,
    ) -> Result<(), String> {
        let outcome = write.wait_blocking();
        if outcome.as_ref().is_err_and(writer::withdrew_error) {
            self.roll_back(at, started);
        }
        outcome
    }

    /// `record` as one entry line, or the whole store when the entries would
    /// outgrow their snapshot.
    fn encode_entry(&mut self, record: &DurableInvocation) -> Result<Staged, String> {
        let mut bytes = serde_json::to_vec(&EntryRef {
            version: self.snapshot.version,
            record,
        })
        .map_err(|_| "invocation journal encoding failed")?;
        bytes.push(b'\n');
        let Some(tail) = self
            .tail
            .as_mut()
            .filter(|tail| tail.takes(bytes.len() as u64))
        else {
            return self.encode_snapshot();
        };
        tail.len = tail.len.saturating_add(bytes.len() as u64);
        Ok(Staged::Append(bytes))
    }

    fn encode_snapshot(&mut self) -> Result<Staged, String> {
        self.tail = None;
        let mut bytes =
            serde_json::to_vec(&self.snapshot).map_err(|_| "invocation journal encoding failed")?;
        bytes.push(b'\n');
        if bytes.len() as u64 > Self::MAX_BYTES {
            return Err("invocation journal storage limit reached".into());
        }
        let len = bytes.len() as u64;
        self.tail = Some(Tail { len, snapshot: len });
        Ok(Staged::Snapshot(bytes))
    }

    /// Hands `write` to the writer, after every write staged before it. While
    /// the journal opens, before its writer starts, the write is made here.
    fn stage(&mut self, write: Staged, injected: Injected) -> Result<Durable, String> {
        #[cfg(test)]
        let injected = Injected {
            fail: injected.fail.or(self.fail_at),
            hold: self.hold_writes.clone(),
            unconfirmed: std::mem::take(&mut self.unconfirmed_next),
            ..injected
        };
        if let Some(writer) = &self.writer {
            return Ok(Durable::of(writer.stage(write, injected)));
        }
        let Staged::Snapshot(bytes) = write else {
            return Err("invocation journal appended before it opened".into());
        };
        let file = writer::write_snapshot(&self.root, &bytes, &injected, &self.counted)?;
        if let Some((tail, _)) = self.opening.as_mut() {
            *tail = Some(file);
        }
        Ok(Durable::settled())
    }

    /// What this writer asked of the disk so far.
    #[cfg(test)]
    pub(super) fn barriers(&self) -> Barriers {
        self.counted.barriers()
    }

    /// Skip the barriers themselves, for tests that measure size.
    #[cfg(test)]
    pub(super) fn elide_barriers(&self) {
        self.counted.elide();
    }
}

/// The durable store as a snapshot value: what a restart replays before
/// recovery. It reads the file only; a live writer keeps its lock.
#[cfg(test)]
pub(super) fn durable_snapshot(root: &Path) -> serde_json::Value {
    let mut file = File::open(root.join("records.json")).expect("invocation journal store");
    let loaded = Loaded::read(&mut file).expect("decodable invocation journal store");
    serde_json::to_value(&loaded.snapshot).expect("invocation journal snapshot value")
}

fn private_file(file: &File) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|_| "invocation journal file unreadable")?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o777 != 0o600 {
        return Err("invocation journal requires private regular files".into());
    }
    Ok(())
}

pub(super) fn unknown(invocation: InvocationId) -> InvocationUpdate {
    InvocationUpdate::Failed {
        invocation,
        error: tilo_actions::ActionFailure {
            code: "action_outcome_unknown".into(),
            message:
                "The application outcome is unknown; this invocation will not be executed again."
                    .into(),
            details: None,
            retryable: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY_SNAPSHOT: &str = r#"{"version":1,"host":"00000000-0000-0000-0000-000000000063","records":[{"key":{"room":"00000000-0000-0000-0000-000000000001","host":"00000000-0000-0000-0000-000000000063","owner":{"Local":{"actor":"person"}},"invocation":"00000000-0000-0000-0000-000000000004"},"intent":{},"wire_id":1,"update":{"state":"succeeded","invocation":"00000000-0000-0000-0000-000000000004","output":{"value":"legacy"}},"uncertain":false,"cancellation_requested":false}]}"#;
    const ENROLLED_SNAPSHOT: &str = r#"{"version":2,"host":"00000000-0000-0000-0000-000000000063","records":[{"key":{"room":"00000000-0000-0000-0000-000000000001","host":"00000000-0000-0000-0000-000000000063","owner":{"Enrolled":{"root":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"device":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"task":"00000000-0000-0000-0000-000000000005","run":"00000000-0000-0000-0000-000000000006","channel":"00000000-0000-0000-0000-000000000007"}},"invocation":"00000000-0000-0000-0000-000000000004"},"intent":{},"wire_id":1,"update":{"state":"succeeded","invocation":"00000000-0000-0000-0000-000000000004","output":{"value":"private result"}},"uncertain":false,"cancellation_requested":false}]}"#;

    /// A first open cut short after the directory and its lock exist, but
    /// before the first snapshot is written (the process was killed), left a
    /// store that was never initialized. Opening it again initializes it;
    /// refusing it left the desktop without collaboration on every start.
    #[test]
    fn a_store_whose_first_open_was_cut_short_is_initialized() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        drop(InvocationJournal::open(&root).unwrap());
        std::fs::remove_file(root.join("records.json")).unwrap();
        let mut journal = InvocationJournal::open(&root).expect("an uninitialized store opens");
        assert!(journal.snapshot.records.is_empty());
        let mut local = key();
        local.host = journal.host();
        local.owner = InvocationOwner::Local {
            actor: "person".into(),
        };
        journal.admit(local, serde_json::json!({}), 1).unwrap();
        drop(journal);
        assert!(
            root.join("records.json").exists(),
            "the store keeps its snapshot"
        );
        assert_eq!(
            InvocationJournal::open(&root)
                .unwrap()
                .snapshot
                .records
                .len(),
            1,
            "and reads it back"
        );
    }

    #[test]
    fn enrolled_journal_literal_versions_retain_owner_and_writer_floor() {
        for (literal, version) in [(LEGACY_SNAPSHOT, 1), (ENROLLED_SNAPSHOT, 2)] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("journal");
            drop(InvocationJournal::open(&root).unwrap());
            std::fs::write(root.join("records.json"), literal).unwrap();
            let mut journal = InvocationJournal::open(&root).expect("supported private ownership");
            let before: serde_json::Value = serde_json::from_str(literal).unwrap();
            assert_eq!(serde_json::to_value(&journal.snapshot).unwrap(), before);
            let mut local = key();
            local.host = journal.host();
            local.owner = InvocationOwner::Local {
                actor: "person".into(),
            };
            journal.admit(local, serde_json::json!({}), 2).unwrap();
            assert_eq!(
                journal.snapshot.version, version,
                "legacy writes never downgrade a private store"
            );
            drop(journal);
            let recovered = InvocationJournal::open(&root).unwrap();
            assert_eq!(
                serde_json::to_value(&recovered.snapshot.records[0]).unwrap(),
                before["records"][0]
            );
            if version == 2 {
                let mut foreign = recovered.snapshot.records[0].key.clone();
                for owner in [
                    InvocationOwner::Local {
                        actor: "person".into(),
                    },
                    InvocationOwner::Certified {
                        root: [1; 32],
                        device: [2; 32],
                    },
                    InvocationOwner::Certified {
                        root: [1; 32],
                        device: [3; 32],
                    },
                ] {
                    foreign.owner = owner;
                    assert!(
                        recovered.lookup(&foreign).unwrap().is_none(),
                        "private recovered result cannot alias a public owner"
                    );
                }
            }
        }
    }

    #[test]
    fn enrolled_journal_old_owner_decoder_rejects_private_rows() {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        enum VersionOneOwner {
            Local { actor: String },
            Certified { root: [u8; 32], device: [u8; 32] },
        }
        let legacy: serde_json::Value = serde_json::from_str(LEGACY_SNAPSHOT).unwrap();
        let old =
            serde_json::from_value::<VersionOneOwner>(legacy["records"][0]["key"]["owner"].clone())
                .unwrap();
        assert!(matches!(old, VersionOneOwner::Local { actor } if actor == "person"));
        let certified: VersionOneOwner = serde_json::from_value(
            serde_json::to_value(InvocationOwner::Certified {
                root: [1; 32],
                device: [2; 32],
            })
            .unwrap(),
        )
        .unwrap();
        assert!(
            matches!(certified, VersionOneOwner::Certified { root, device } if root == [1;32] && device == [2;32])
        );
        let private: serde_json::Value = serde_json::from_str(ENROLLED_SNAPSHOT).unwrap();
        assert_ne!(private["version"], 1, "old snapshot gate fails explicitly");
        assert!(
            serde_json::from_value::<VersionOneOwner>(
                private["records"][0]["key"]["owner"].clone()
            )
            .is_err(),
            "old enum never silently classifies private ownership as Local"
        );
    }

    #[test]
    fn enrolled_journal_upgrade_barrier_never_returns_effect_permission_on_failure() {
        let private: Snapshot =
            serde_json::from_str(ENROLLED_SNAPSHOT).expect("private owner data");
        for failure in [BarrierFailure::File, BarrierFailure::Directory] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("journal");
            let mut journal = InvocationJournal::open(&root).unwrap();
            let mut key = private.records[0].key.clone();
            key.host = journal.host();
            journal.fail_at = Some(failure);
            assert!(
                journal
                    .admit(key.clone(), serde_json::json!({}), 1)
                    .is_err()
            );
            assert!(
                journal
                    .admit(key.clone(), serde_json::json!({}), 2)
                    .is_err()
            );
            drop(journal);
            let reopened = InvocationJournal::open(&root).unwrap();
            if failure == BarrierFailure::Directory {
                assert_eq!(reopened.snapshot.version, 2);
                let retained = reopened.lookup(&key).unwrap().unwrap();
                assert!(retained.uncertain);
                assert_eq!(retained.key.owner, key.owner);
            } else {
                assert_eq!(reopened.snapshot.version, 1);
                assert!(reopened.lookup(&key).unwrap().is_none());
            }
        }
    }

    #[test]
    fn enrolled_journal_rejects_private_downgrade_and_noncanonical_ownership() {
        for literal in [
            ENROLLED_SNAPSHOT.replace("\"version\":2", "\"version\":1"),
            ENROLLED_SNAPSHOT.replace(
                "00000000-0000-0000-0000-000000000005",
                "00000000000000000000000000000005",
            ),
            ENROLLED_SNAPSHOT.replace(
                "00000000-0000-0000-0000-000000000006",
                "00000000-0000-0000-0000-000000000000",
            ),
            ENROLLED_SNAPSHOT.replace("00000000-0000-0000-0000-000000000007", &"x".repeat(200)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("journal");
            drop(InvocationJournal::open(&root).unwrap());
            std::fs::write(root.join("records.json"), literal).unwrap();
            assert!(InvocationJournal::open(&root).is_err());
        }
    }

    fn key() -> InvocationKey {
        InvocationKey {
            room: "5ca720b0-7649-4bdc-9c30-77380f7c2be5".into(),
            host: uuid::Uuid::from_u128(99),
            owner: InvocationOwner::Certified {
                root: [1; 32],
                device: [2; 32],
            },
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(
                0xa5a5_a5a5_0000_0000_0000_0000_0000_0001,
            )),
        }
    }

    #[test]
    fn invocation_journal_initialization_parent_barrier_must_succeed_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let attempts = std::cell::Cell::new(0);
        for expected in 1..=2 {
            let opened = InvocationJournal::open_with_parent_barrier(&root, |parent| {
                assert_eq!(parent, dir.path());
                attempts.set(attempts.get() + 1);
                Err(std::io::Error::other("injected parent sync failure"))
            });
            assert!(
                opened.is_err(),
                "no writer or dispatch permit before parent sync"
            );
            assert_eq!(
                attempts.get(),
                expected,
                "reopen retries the failed barrier"
            );
            assert!(
                root.join("records.json").is_file(),
                "failure leaves a valid store"
            );
        }
        let mut journal = InvocationJournal::open_with_parent_barrier(&root, |parent| {
            attempts.set(attempts.get() + 1);
            File::open(parent)?.sync_all()
        })
        .unwrap();
        assert_eq!(attempts.get(), 3);
        let mut k = key();
        k.host = journal.host();
        assert!(journal.admit(k, serde_json::json!({}), 1).unwrap());
    }

    #[test]
    fn invocation_journal_rejects_preexisting_cross_owner_uuid_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let mut journal = InvocationJournal::open(&root).unwrap();
        let mut k = key();
        k.host = journal.host();
        journal.admit(k, serde_json::json!({}), 1).unwrap();
        let mut conflicting = journal.snapshot.records[0].clone();
        conflicting.key.owner = InvocationOwner::Local {
            actor: "other".into(),
        };
        conflicting.wire_id = 2;
        journal.snapshot.records.push(conflicting);
        journal.compact().unwrap();
        drop(journal);
        assert!(
            InvocationJournal::open(&root).is_err(),
            "an older ambiguous store cannot authorize either application UUID owner"
        );
    }

    #[test]
    fn invocation_journal_restart_retains_terminal_and_never_reissues_admission() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let mut journal = InvocationJournal::open(&root).unwrap();
        let mut k = key();
        k.host = journal.host();
        let intent = serde_json::json!({"argument": null, "action": "write"});
        let update = tilo_actions::InvocationUpdate::Succeeded {
            invocation: k.invocation,
            output: tilo_actions::ActionOutput {
                value: Some(serde_json::json!({"private": "result"})),
                resources: vec![],
            },
        };
        assert!(journal.admit(k.clone(), intent.clone(), 1).unwrap());
        assert!(!journal.admit(k.clone(), intent.clone(), 1).unwrap());
        assert!(
            journal
                .admit(
                    k.clone(),
                    serde_json::json!({"argument":"", "action":"write"}),
                    2
                )
                .is_err()
        );
        journal.update(&k, update.clone(), false).unwrap();
        drop(journal);
        let mut journal = InvocationJournal::open(&root).unwrap();
        assert!(!journal.admit(k.clone(), intent, 2).unwrap());
        assert_eq!(journal.lookup(&k).unwrap().unwrap().update, update);
    }

    #[test]
    fn invocation_journal_recovery_of_admission_is_uncertain_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let mut journal = InvocationJournal::open(&root).unwrap();
        let mut k = key();
        k.host = journal.host();
        journal
            .admit(k.clone(), serde_json::json!({"argument": "secret"}), 1)
            .unwrap();
        drop(journal);
        let journal = InvocationJournal::open(&root).unwrap();
        let record = journal.lookup(&k).unwrap().unwrap();
        assert!(record.uncertain);
        assert!(
            matches!(record.update, tilo_actions::InvocationUpdate::Failed { error, .. } if error.code == "action_outcome_unknown" && !error.retryable)
        );
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(root.join("records.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn invocation_journal_lock_and_invalid_stores_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let journal = InvocationJournal::open(&root).unwrap();
        assert!(
            InvocationJournal::open(&root).is_err(),
            "second process/file owner must be excluded"
        );
        drop(journal);
        std::fs::write(root.join("records.json"), b"{broken").unwrap();
        assert!(InvocationJournal::open(&root).is_err());
        std::fs::write(
            root.join("records.json"),
            br#"{"version":999,"host":"00000000-0000-0000-0000-000000000063","records":[]}"#,
        )
        .unwrap();
        assert!(InvocationJournal::open(&root).is_err());
    }

    #[test]
    fn invocation_journal_excludes_a_second_process() {
        const PROBE: &str = "TILO_INVOCATION_JOURNAL_LOCK_PROBE";
        if let Some(root) = std::env::var_os(PROBE) {
            assert!(InvocationJournal::open(Path::new(&root)).is_err());
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let _journal = InvocationJournal::open(&root).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg("collab_broker_capnp::invocation_journal::tests::invocation_journal_excludes_a_second_process")
            .env(PROBE, &root).output().unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
    }

    #[test]
    fn invocation_journal_directory_barrier_failure_never_reopens_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let mut journal = InvocationJournal::open(&root).unwrap();
        journal.fail_at = Some(BarrierFailure::Directory);
        let mut k = key();
        k.host = journal.host();
        assert!(
            journal
                .admit(k.clone(), serde_json::json!({"argument": "secret"}), 1)
                .is_err()
        );
        assert!(
            journal
                .admit(k.clone(), serde_json::json!({"argument": "secret"}), 2)
                .is_err(),
            "poisoned writer cannot retry"
        );
        drop(journal);
        let journal = InvocationJournal::open(&root).unwrap();
        assert!(
            journal.lookup(&k).unwrap().unwrap().uncertain,
            "renamed admission survives but never creates another permit"
        );
    }
}

#[cfg(test)]
#[path = "invocation_journal/guarded_tests.rs"]
mod guarded_tests;

#[cfg(test)]
#[path = "invocation_journal/authenticated_local_tests.rs"]
mod authenticated_local_tests;

#[cfg(test)]
#[path = "invocation_journal/append_tests.rs"]
mod append_tests;
