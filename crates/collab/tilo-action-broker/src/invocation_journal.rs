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
use std::io::{Read, Seek, SeekFrom, Write};
#[path = "invocation_journal/effect.rs"]
mod effect;
#[path = "invocation_journal/guarded.rs"]
mod guarded;
pub(crate) use effect::PreparedEffect;
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
                || !task.starts_with("task:")
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

/// The open store, positioned after its last acknowledged entry.
struct Tail {
    file: File,
    /// Durable length: the snapshot plus every acknowledged entry.
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
    _lock: advisory_lock::FileLock,
    snapshot: Snapshot,
    index: Index,
    /// The store open for appends; `None` makes the next transition compact
    /// (an earlier writer's snapshot ends without an entry separator).
    tail: Option<Tail>,
    poisoned: bool,
    #[cfg(test)]
    pub fail_at: Option<BarrierFailure>,
    #[cfg(test)]
    pub barriers: Barriers,
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

/// What this writer asked of the disk. `elide` skips the barriers themselves
/// for tests that measure size rather than durability.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Barriers {
    pub writes: u64,
    pub file: u64,
    pub directory: u64,
    pub elide: bool,
}

/// Performs one durability barrier; under test it is counted, and elided
/// when a size test asks.
macro_rules! barrier {
    ($barriers:expr, $kind:ident, $barrier:expr) => {{
        #[cfg(test)]
        let elide = {
            $barriers.$kind = $barriers.$kind.saturating_add(1);
            $barriers.elide
        };
        #[cfg(not(test))]
        let elide = false;
        if elide { Ok(()) } else { $barrier }
    }};
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
        let loaded = if created {
            Loaded::empty(uuid::Uuid::new_v4())
        } else {
            // Opened for appends too: a clean store keeps exactly this file.
            let mut file = OpenOptions::new()
                .read(true)
                .append(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(root.join("records.json"))
                .map_err(|_| "invocation journal snapshot missing or unreadable")?;
            private_file(&file)?;
            let loaded = Loaded::read(&mut file)?;
            observed_store = Some(file);
            loaded
        };
        let mut journal = Self {
            root: root.to_owned(),
            _lock: lock,
            snapshot: loaded.snapshot,
            index: loaded.index,
            tail: None,
            poisoned: false,
            #[cfg(test)]
            fail_at: None,
            #[cfg(test)]
            barriers: Barriers::default(),
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
            journal.tail = observed_store
                .filter(|_| loaded.appendable)
                .map(|file| Tail {
                    file,
                    len: loaded.len,
                    snapshot: loaded.len,
                });
        }
        // A failed initialization may leave a valid snapshot. Its existence
        // does not prove the directory entry was durable: retry on every open.
        parent_barrier(root.parent().ok_or("invocation journal needs a parent")?)
            .map_err(|_| "invocation journal parent barrier failed")?;
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
        Ok((at, record))
    }

    pub fn lookup(&self, key: &InvocationKey) -> Result<Option<DurableInvocation>, String> {
        self.check()?;
        Ok(self
            .position(key)
            .and_then(|at| self.snapshot.records.get(at))
            .cloned())
    }

    /// A true return is the sole dispatch permit. Caller must serialize the
    /// authorization / confirmation / admission / dispatch transaction.
    pub fn admit(
        &mut self,
        key: InvocationKey,
        intent: serde_json::Value,
        wire_id: u64,
    ) -> Result<bool, String> {
        self.admit_with_evidence(key, intent, wire_id, None)
    }

    /// The evidence and admission share the original append and data barrier.
    pub(super) fn admit_with_evidence(
        &mut self,
        key: InvocationKey,
        intent: serde_json::Value,
        wire_id: u64,
        admission: Option<crate::action_evidence::AdmissionEvidence>,
    ) -> Result<bool, String> {
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
            return Ok(false);
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
        self.commit(at)?;
        #[cfg(test)]
        if self.fail_at.take() == Some(BarrierFailure::AfterAdmission) {
            self.poisoned = true;
            return Err("injected admission-before-delivery crash".into());
        }
        Ok(true)
    }

    /// A terminal outcome is durable before this returns. Nonterminal
    /// progress is kept for observers only: recovery discards it anyway.
    pub fn update(
        &mut self,
        key: &InvocationKey,
        update: InvocationUpdate,
        uncertain: bool,
    ) -> Result<(), String> {
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
        if terminal { self.commit(at) } else { Ok(()) }
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
        record.guarded = Some(guarded::State::selected(selection));
        self.snapshot.version = self.snapshot.version.max(3);
        self.commit(at)
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
        record.update = guarded::unavailable(key.invocation);
        record.uncertain = false;
        self.commit(at)
    }

    pub fn cancel(&mut self, key: &InvocationKey) -> Result<(), String> {
        self.check()?;
        let (at, record) = self.record_mut(key)?;
        if record.update.is_terminal() {
            return Err("invocation is no longer running".into());
        }
        if record.guarded.is_some() {
            return Err("guarded reads are noncancellable".into());
        }
        record.cancellation_requested = true;
        self.commit(at)
    }

    pub(super) fn check(&self) -> Result<(), String> {
        if self.poisoned {
            Err("invocation journal requires recovery after a failed barrier".into())
        } else {
            Ok(())
        }
    }

    /// Makes record `at` durable as it now stands: one appended entry, or a
    /// compaction once the entries would outgrow the snapshot.
    fn commit(&mut self, at: usize) -> Result<(), String> {
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
        let outcome = self.write_entry(at);
        if outcome.is_err() {
            self.poison();
        }
        outcome
    }

    /// Rewrites the whole store as one snapshot through an atomic rename.
    fn compact(&mut self) -> Result<(), String> {
        let outcome = self.write_snapshot();
        if outcome.is_err() {
            self.poison();
        }
        outcome
    }

    fn poison(&mut self) {
        self.poisoned = true;
        self.tail = None;
    }

    fn write_entry(&mut self, at: usize) -> Result<(), String> {
        let record = self
            .snapshot
            .records
            .get(at)
            .ok_or("unknown invocation journal identity")?;
        let mut bytes = serde_json::to_vec(&EntryRef {
            version: self.snapshot.version,
            record,
        })
        .map_err(|_| "invocation journal encoding failed")?;
        bytes.push(b'\n');
        let Some(mut tail) = self
            .tail
            .take()
            .filter(|tail| tail.takes(bytes.len() as u64))
        else {
            return self.write_snapshot();
        };
        #[cfg(test)]
        {
            self.barriers.writes = self.barriers.writes.saturating_add(1);
        }
        let mut durable = tail
            .file
            .write_all(&bytes)
            .map_err(|_| "invocation journal write failed");
        #[cfg(test)]
        if durable.is_ok() && self.fail_at == Some(BarrierFailure::File) {
            durable = Err("injected file barrier failure");
        }
        if durable.is_ok() {
            durable = barrier!(self.barriers, file, tail.file.sync_data())
                .map_err(|_| "invocation journal file barrier failed");
        }
        if let Err(error) = durable {
            // No caller was told of it: take the bytes back if the disk lets us.
            let _ = tail.file.set_len(tail.len);
            return Err(error.into());
        }
        tail.len = tail.len.saturating_add(bytes.len() as u64);
        self.tail = Some(tail);
        #[cfg(test)]
        if self.fail_at == Some(BarrierFailure::Directory) {
            return Err("injected failure after a durable entry".into());
        }
        Ok(())
    }

    fn write_snapshot(&mut self) -> Result<(), String> {
        self.tail = None;
        let mut bytes =
            serde_json::to_vec(&self.snapshot).map_err(|_| "invocation journal encoding failed")?;
        bytes.push(b'\n');
        if bytes.len() as u64 > Self::MAX_BYTES {
            return Err("invocation journal storage limit reached".into());
        }
        let temporary = self.root.join(format!(".write-{}", uuid::Uuid::new_v4()));
        let result: Result<File, &'static str> = (|| {
            // The renamed file stays open: later entries append to exactly it.
            let mut file = OpenOptions::new()
                .append(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temporary)
                .map_err(|_| "invocation journal temporary file unavailable")?;
            #[cfg(test)]
            {
                self.barriers.writes = self.barriers.writes.saturating_add(1);
            }
            file.write_all(&bytes)
                .map_err(|_| "invocation journal write failed")?;
            #[cfg(test)]
            if self.fail_at == Some(BarrierFailure::File) {
                return Err("injected file barrier failure");
            }
            barrier!(self.barriers, file, file.sync_all())
                .map_err(|_| "invocation journal file barrier failed")?;
            std::fs::rename(&temporary, self.root.join("records.json"))
                .map_err(|_| "invocation journal rename failed")?;
            #[cfg(test)]
            if self.fail_at == Some(BarrierFailure::Directory) {
                return Err("injected directory barrier failure");
            }
            barrier!(
                self.barriers,
                directory,
                File::open(&self.root).and_then(|directory| directory.sync_all())
            )
            .map_err(|_| "invocation journal directory barrier failed")?;
            Ok(file)
        })();
        let _ = std::fs::remove_file(&temporary);
        let len = bytes.len() as u64;
        self.tail = Some(Tail {
            file: result?,
            len,
            snapshot: len,
        });
        Ok(())
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
