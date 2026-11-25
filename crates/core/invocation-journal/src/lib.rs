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
//! A store of a version this build does not write is refused, not read: what
//! rows a format holds is what its checks allow, so reading one the checks do
//! not cover would admit rows this build would not have written.

pub mod action_evidence;
mod evidence_signer;
#[cfg(any(test, feature = "test-support"))]
pub use evidence_signer::BEFORE_SIGN;

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
mod content_puts;
mod effect;
#[cfg(any(test, feature = "test-support"))]
pub use content_puts::CONTENT_PUTS_FILE;
pub use content_puts::ContentHolds;
pub use content_puts::ContentPutRecord;
#[cfg(any(test, feature = "test-support"))]
pub use content_puts::attributions;
mod writer;
pub use effect::PreparedEffect;
#[cfg(any(test, feature = "test-support"))]
pub use writer::Barriers;
pub use writer::Durable;
use writer::{Counted, Injected, Write as Staged, Writer};
#[cfg(test)]
mod effect_tests;
use std::path::{Path, PathBuf};
use tilo_actions::{InvocationId, InvocationUpdate};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum InvocationOwner {
    Local {
        actor: tilo_action_policy::Actor,
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
        /// The agent this session was delegated to, or `null` for the person.
        /// The field must be present: reading an omission as the person would
        /// hand a delegated agent's invocation to them.
        #[serde(default = "omitted_agent")]
        agent: Option<String>,
    },
}

/// The value an omitted `agent` deserializes to. Serde's default for an
/// `Option` field is `None`, which would read a dropped field as the person
/// themselves; this sentinel cannot be a real agent name, so
/// [`Self::validate`] (and [`crate::action_evidence::verify`]) refuses it
/// instead.
#[allow(
    clippy::unnecessary_wraps,
    reason = "serde's field default must match the Option field type"
)]
fn omitted_agent() -> Option<String> {
    Some(String::from(OMITTED_AGENT))
}

/// The omission sentinel. Never a valid agent: it holds a control character,
/// which every agent-name check refuses.
const OMITTED_AGENT: &str = "\u{0}omitted-agent";

impl InvocationOwner {
    pub fn verified_actor(&self) -> Option<crate::action_evidence::VerifiedActor> {
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
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        if let Self::AuthenticatedLocal {
            agent: Some(agent), ..
        } = self
            && (agent.is_empty()
                || agent.len() > 512
                || agent.bytes().any(|byte| byte.is_ascii_control()))
        {
            return Err("invalid authenticated local invocation agent".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationKey {
    pub workspace: String,
    pub host: uuid::Uuid,
    pub owner: InvocationOwner,
    pub invocation: InvocationId,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DurableInvocation {
    pub key: InvocationKey,
    /// The admission intent. Its `request`, when it had one, is retained only
    /// as `request_digest`: ask [`Self::admitted_request_is`], never this map.
    pub intent: serde_json::Value,
    request_digest: Option<Digest>,
    pub wire_id: u64,
    pub update: InvocationUpdate,
    pub uncertain: bool,
    pub cancellation_requested: bool,
    pub prepared_effect: Option<PreparedEffect>,
    pub admission: Option<crate::action_evidence::AdmissionEvidence>,
    pub terminal: Option<crate::action_evidence::TerminalReceipt>,
}

impl DurableInvocation {
    /// Whether `request` is the request this invocation was admitted with.
    /// A record without one compares as `null`, like an absent JSON member.
    pub fn admitted_request_is(&self, request: &serde_json::Value) -> bool {
        match (&self.request_digest, self.intent.get("request")) {
            (Some(digest), _) => *digest == request_digest(request),
            (None, Some(retained)) => retained == request,
            (None, None) => request.is_null(),
        }
    }

    /// Whether `intent` is the whole intent this invocation was admitted with.
    pub fn admitted_intent_is(&self, intent: &serde_json::Value) -> bool {
        if self.request_digest.is_none() {
            return self.intent == *intent;
        }
        let (rest, digest) = retained_intent(intent.clone());
        digest == self.request_digest && rest == self.intent
    }
}

/// Splits an admission intent into what is retained verbatim and the digest
/// of its `request`, the one member that carries the caller's arguments.
fn retained_intent(mut intent: serde_json::Value) -> (serde_json::Value, Option<Digest>) {
    let request = intent
        .as_object_mut()
        .and_then(|fields| fields.remove("request"));
    (intent, request.as_ref().map(request_digest))
}

/// The journal's exact retry digest, also bound by private admission evidence.
pub fn admission_request_digest(request: &serde_json::Value) -> [u8; 32] {
    runtime_collab::admission_request_digest(request)
}

/// A digest of `request` that is equal exactly when the JSON values are:
/// object members are hashed in key order, whatever order they arrived in.
fn request_digest(request: &serde_json::Value) -> Digest {
    Digest(runtime_collab::admission_request_digest(request))
}

/// The store version this build writes and the only one it reads.
const VERSION: u32 = 7;

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

    /// Takes the record at `at` out of `records` and out of the index, as
    /// though it had never been inserted.
    fn remove(&mut self, records: &mut Vec<DurableInvocation>, at: usize) {
        if at >= records.len() {
            return;
        }
        let record = records.remove(at);
        self.by_invocation.remove(&record.key.invocation);
        self.wire_ids.remove(&record.wire_id);
        for position in self.by_invocation.values_mut() {
            if *position > at {
                *position = position.saturating_sub(1);
            }
        }
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
                version: VERSION,
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

    /// Whether `file` is a store of an earlier version that holds no invocation. A later
    /// version's is never replaced: it is a newer Tilo's.
    fn is_earlier_and_empty(file: &File) -> bool {
        #[derive(Deserialize)]
        struct Head {
            version: u32,
            records: Vec<serde::de::IgnoredAny>,
        }
        let mut bytes = Vec::new();
        let read = Read::by_ref(&mut &*file)
            .take(InvocationJournal::MAX_BYTES)
            .read_to_end(&mut bytes);
        let mut values = serde_json::Deserializer::from_slice(&bytes).into_iter::<Head>();
        read.is_ok()
            && matches!(values.next(), Some(Ok(head)) if head.version < VERSION && head.records.is_empty())
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
        let snapshot: Snapshot = serde_json::from_slice(head)
            .map_err(|_| "invocation journal snapshot is corrupt or unsupported")?;
        if snapshot.version != VERSION || snapshot.host.is_nil() {
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
            check_resource(&record, loaded.snapshot.host)?;
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
            if line.len() as u64 > InvocationJournal::MAX_ENTRY {
                return Err("invocation journal entry exceeds storage limit".into());
            }
            let entry: Entry = serde_json::from_slice(line)
                .map_err(|_| "invocation journal entry is corrupt or unsupported")?;
            loaded.replay(entry)?;
        }
        Ok(loaded)
    }

    fn replay(&mut self, entry: Entry) -> Result<(), String> {
        if entry.version != VERSION {
            return Err("invocation journal entry version unsupported".into());
        }
        check_resource(&entry.record, self.snapshot.host)?;
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

/// Every check one stored row must pass.
pub fn evidence_facts(
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
        workspace: key.workspace.clone(),
        invocation: key.invocation,
        actor: key
            .owner
            .verified_actor()
            .ok_or("legacy actor cannot attest a user")?,
        // The admission names its actor as the broker decided it; only a member's call carries
        // attested evidence.
        actor_id: match tilo_actions::Actor::deserialize(
            intent.get("actor").ok_or("admission actor missing")?,
        )
        .map_err(|error| format!("admission actor {} does not read: {error}", intent["actor"]))?
        {
            tilo_actions::Actor::Member(member) => member,
            _ => return Err("attested admission names no member".into()),
        },
        app_id: text("app_id")?,
        document_id: text("document_id")?,
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
    let expected = evidence_facts(key, intent)?;
    evidence.validate(&expected)?;
    evidence.validate_retained(intent)
}

fn check_resource(record: &DurableInvocation, host: uuid::Uuid) -> Result<(), String> {
    record.key.owner.validate()?;
    if let Some(evidence) = &record.admission {
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
    if let Some(prepared) = &record.prepared_effect {
        prepared.validate(record)?;
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

pub struct InvocationJournal {
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
    /// The content put attributions beside the store, as staged; `None` only
    /// while the journal opens, before its file is read.
    content_puts: Option<content_puts::ContentPutsStore>,
    counted: Counted,
    /// What a reader may see of each invocation with a write in flight: as
    /// it was before the first, until each write is durable.
    undurable: HashMap<InvocationId, Undurable>,
    /// The record [`Self::document_mut`] last handed out, as it was before.
    before: Option<DurableInvocation>,
    poisoned: bool,
    #[cfg(any(test, feature = "test-support"))]
    pub fail_at: Option<BarrierFailure>,
    /// One-shot: the next write is made durable but reported as failed.
    #[cfg(any(test, feature = "test-support"))]
    pub unconfirmed_next: bool,
    /// One-shot pause after durable admission, while the writer is still held.
    #[cfg(any(test, feature = "test-support"))]
    pub after_admission: Option<Box<dyn FnOnce() + Send>>,
    /// Run before each write starts, on the thread making it, as a slow
    /// disk would hold it.
    #[cfg(any(test, feature = "test-support"))]
    pub hold_writes: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

/// An invocation's writes in flight: readers see `durable` until a write
/// resolves, then the record that write made durable.
struct Undurable {
    durable: Option<DurableInvocation>,
    writes: Vec<(DurableInvocation, std::sync::Arc<writer::Ticket>)>,
    /// Whether the first of `writes` admits the invocation: before it the
    /// store held no record of it at all.
    admission: bool,
}

impl Undurable {
    /// Whether the admission these writes began with left nothing durable,
    /// so the invocation was never admitted.
    fn admission_withdrawn(&self) -> bool {
        self.admission
            && self
                .writes
                .first()
                .is_some_and(|(_, ticket)| ticket.outcome().as_ref().is_some_and(writer::withdrew))
    }
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
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BarrierFailure {
    File,
    Directory,
    AfterAdmission,
    Terminal,
}

/// `update` as the journal keeps a call whose action answers a secret
/// ([`tilo_actions::SECRET_ANSWER_TAG`]): only that it was given. A record kept before the
/// declaration carried the tag is rewritten the same way when the store is recovered, so no
/// credential lies in the file or in the entries it rewrites.
fn secret_answer_left_out(
    action: Option<&serde_json::Value>,
    update: InvocationUpdate,
) -> InvocationUpdate {
    let answers = action
        .and_then(serde_json::Value::as_str)
        .is_some_and(tilo_actions::remote::answers_a_secret);
    match update {
        InvocationUpdate::Succeeded { invocation, .. } if answers => InvocationUpdate::Succeeded {
            invocation,
            output: tilo_actions::ActionOutput {
                value: Some(serde_json::json!({ "given": true })),
                resources: Vec::new(),
            },
        },
        update => update,
    }
}

impl InvocationJournal {
    const MAX_BYTES: u64 = 256 * 1024 * 1024;
    /// One appended entry: a record with its intent, update and prepared effect.
    const MAX_ENTRY: u64 = 4 * 1024 * 1024;
    /// Entries may always grow to this before a compaction is due.
    const COMPACT_AFTER: u64 = 1024 * 1024;

    pub fn open(root: &Path) -> Result<Self, String> {
        Self::open_with_parent_barrier(root, |parent| {
            tilo_files::native::atomic::sync_directory(parent)
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
        directory_barrier: impl FnOnce(&File, &File) -> std::io::Result<()>,
    ) -> Result<Self, String> {
        let created = match std::fs::create_dir(root) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(_) => return Err("invocation journal directory unavailable".into()),
        };
        // Retain the directory actually opened, and checked private, for this
        // snapshot's reconciliation, rather than reopening its pathname after
        // decoding.
        let directory = tilo_files::native::private::open_directory(root)
            .map_err(|_| "invocation journal requires a private directory")?;
        let lock = tilo_files::native::private::lock_file(&root.join("owner.lock"))
            .map_err(|_| "invocation journal lock unavailable")?;
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
            match tilo_files::native::private::append(&root.join("records.json")) {
                Ok(file) => Some(file),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err("invocation journal snapshot missing or unreadable".into()),
            }
        };
        // An earlier Tilo's store that holds no invocation is replaced: there is nothing in it
        // to keep, and refusing it stopped the daemon of every computer that Tilo set up.
        let opened = opened.filter(|file| !Loaded::is_earlier_and_empty(file));
        let created = opened.is_none();
        let loaded = match opened {
            None => Loaded::empty(uuid::Uuid::new_v4()),
            Some(mut file) => {
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
            content_puts: None,
            counted: Counted::default(),
            undurable: HashMap::new(),
            before: None,
            poisoned: false,
            #[cfg(any(test, feature = "test-support"))]
            fail_at: None,
            #[cfg(any(test, feature = "test-support"))]
            unconfirmed_next: false,
            #[cfg(any(test, feature = "test-support"))]
            after_admission: None,
            #[cfg(any(test, feature = "test-support"))]
            hold_writes: None,
        };
        // The attributions are read before anything appends: a line its last
        // writer could not confirm is dropped here, and the store as read is
        // what the next put compacts from.
        journal.content_puts = Some(content_puts::ContentPutsStore::read(root)?);
        let mut recovered = false;
        let mut redacted = false;
        for record in &mut journal.snapshot.records {
            // A record kept before its action's declaration carried the tag keeps none of the
            // answer either: the snapshot it is rewritten into holds no credential.
            let left_out =
                secret_answer_left_out(record.intent.get("action"), record.update.clone());
            if left_out != record.update {
                record.update = left_out;
                redacted = true;
            }
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
        if created || recovered || redacted || loaded.entries > 0 || loaded.torn {
            // Fold the replayed entries (dropping a torn one) and the recovered
            // records into one fresh snapshot before anything is exposed.
            journal.compact()?;
        } else {
            if loaded.appendable
                || journal
                    .snapshot
                    .records
                    .iter()
                    .any(|record| record.prepared_effect.is_some())
            {
                // The decoded file may be the result of a rename whose directory
                // barrier failed. Reconcile the exact decoded file, not a freshly
                // reopened name, before exposing a prepared effect or appending
                // entries a lost rename would orphan.
                directory_barrier(
                    observed_store
                        .as_ref()
                        .ok_or("invocation journal snapshot unavailable")?,
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

    fn document_mut(
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
    pub fn in_flight(&self, invocation: &InvocationId) -> Durable {
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
        let undurable = self.undurable.get(&key.invocation);
        if undurable.is_some_and(Undurable::admission_withdrawn) {
            return Ok(None);
        }
        if let Some(undurable) = undurable.filter(|undurable| !undurable.settled()) {
            return Ok(undurable.visible().cloned());
        }
        Ok(self.snapshot.records.get(at).cloned())
    }

    /// Journal fixtures may exercise admission without signing identities.
    #[cfg(any(test, feature = "test-support"))]
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
    pub fn admit_with_evidence(
        &mut self,
        key: InvocationKey,
        intent: serde_json::Value,
        wire_id: u64,
        admission: Option<crate::action_evidence::AdmissionEvidence>,
    ) -> Result<(bool, Durable), String> {
        self.ready()?;
        self.prune();
        if let Some(evidence) = &admission {
            check_evidence(&key, &intent, evidence)?;
            evidence
                .validate_request_digest(intent.get("request").map(admission_request_digest))?;
            evidence
                .opening_for_owner(&evidence.receipt.facts.actor)?
                .verify(&evidence.receipt, &intent)?;
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
        // different device, workspace, or legacy caller can never alias that state.
        if self.index.by_invocation.contains_key(&key.invocation) {
            return Err("action_invocation_identity_conflict: UUID already reserved".into());
        }
        if key.host != self.host() || wire_id == 0 || self.index.wire_ids.contains(&wire_id) {
            return Err("invocation journal identity conflict".into());
        }
        let update = InvocationUpdate::Accepted {
            invocation: key.invocation,
        };
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
                prepared_effect: None,
                admission,
                terminal: None,
            },
        )?;
        self.before = None;
        #[cfg_attr(
            not(any(test, feature = "test-support")),
            expect(unused_mut, reason = "injections are made only under test")
        )]
        let mut injected = Injected::default();
        #[cfg(any(test, feature = "test-support"))]
        {
            injected.after = self.after_admission.take();
            if self.fail_at == Some(BarrierFailure::AfterAdmission) {
                self.fail_at = None;
                injected.crash_after = true;
            }
        }
        let durable = self.commit_from(at, injected, true)?;
        Ok((true, durable))
    }

    /// [`Self::record_update`], waiting for the write here.
    #[cfg(any(test, feature = "test-support"))]
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
        self.settle(at, &started, write)
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
        self.ready()?;
        let (at, record) = self.document_mut(key)?;
        if record.update.is_terminal() || update.invocation() != key.invocation {
            return Err("invocation journal transition rejected".into());
        }
        if let Some(prepared) = &record.prepared_effect
            && (update != prepared.update || uncertain)
        {
            return Err("prepared effect requires its exact owner-accepted terminal".into());
        }
        let terminal = update.is_terminal();
        record.update = secret_answer_left_out(record.intent.get("action"), update);
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

    /// The cancellation request is durable once the returned write resolves.
    pub fn record_cancellation(&mut self, key: &InvocationKey) -> Result<Durable, String> {
        self.ready()?;
        let (at, record) = self.document_mut(key)?;
        if record.update.is_terminal() {
            return Err("invocation is no longer running".into());
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
    pub fn check(&self) -> Result<(), String> {
        if self.failed() {
            Err("invocation journal requires recovery after a failed barrier".into())
        } else {
            Ok(())
        }
    }

    /// [`Self::check`] for a transition about to be staged: a journal whose
    /// writer stopped at a write that may be durable recovers here first, so
    /// one failed barrier never needs a restart. A journal that could not
    /// stage at all stays failed.
    pub fn ready(&mut self) -> Result<(), String> {
        if !self.poisoned && self.failed() {
            self.recover()?;
        }
        self.check()
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
        if self.writer.is_none() {
            return Err("invocation journal requires recovery after a failed barrier".into());
        }
        self.reopen()
    }

    /// Folds the store as it is on disk into one snapshot, turning every
    /// nonterminal invocation uncertain, exactly as opening it does.
    fn reopen(&mut self) -> Result<(), String> {
        if self.opening.is_some() {
            return Err("invocation journal requires recovery after a failed barrier".into());
        }
        let mut file = tilo_files::native::private::read(&self.root.join("records.json"))
            .map_err(|_| "invocation journal snapshot missing or unreadable")?;
        let loaded = Loaded::read(&mut file)?;
        self.snapshot = loaded.snapshot;
        self.index = loaded.index;
        self.tail = None;
        self.undurable.clear();
        self.before = None;
        self.content_puts = Some(content_puts::ContentPutsStore::read(&self.root)?);
        for record in &mut self.snapshot.records {
            record.update =
                secret_answer_left_out(record.intent.get("action"), record.update.clone());
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
        // The recovered snapshot rewrites the store through the atomic
        // rename a reader only ever sees whole. The writer that holds the
        // store's lock reaches it after every write staged before recovery,
        // all of them stopped, and goes on only once it is durable; waiting
        // for it here means recovery completes before this returns.
        let outcome = self
            .encode_snapshot()
            .map(|snapshot| match snapshot {
                Staged::Snapshot(bytes) => Staged::Recovery(bytes),
                other => other,
            })
            .and_then(|write| self.stage(write, Injected::default()));
        match outcome {
            Ok(durable) => durable.wait_blocking(),
            Err(error) => {
                self.poison();
                Err(error)
            }
        }
    }

    /// Stages record `at` as it now stands for the writer: one appended
    /// entry, or a compaction once the entries would outgrow the snapshot.
    /// Readers keep seeing the record as it was until the write is durable.
    fn commit(&mut self, at: usize, injected: Injected) -> Result<Durable, String> {
        self.commit_from(at, injected, false)
    }

    /// [`Self::commit`] of record `at`, which an `admission` just inserted:
    /// before it there was no record at all.
    fn commit_from(
        &mut self,
        at: usize,
        injected: Injected,
        admission: bool,
    ) -> Result<Durable, String> {
        let before = self.before.take();
        if let Some(record) = self.snapshot.records.get_mut(at)
            && let Some(admission) = &record.admission
            && record.terminal.is_none()
            && let Some(category) =
                crate::action_evidence::terminal_category(&record.update, record.uncertain)
        {
            // Losing signing identity must not lose an application's outcome.
            // No receipt is preferable to falsely certifying a completion;
            // recovery uncertainty is never retroactively signed as success.
            record.terminal =
                match crate::action_evidence::mint_terminal(admission.public_receipt(), category) {
                    Ok(terminal) => Some(terminal),
                    Err(error) => {
                        tracing::warn!(%error, "the invocation's completion has no signed receipt");
                        None
                    }
                };
        }
        let record = self
            .snapshot
            .records
            .get(at)
            .ok_or("unknown invocation journal identity")?
            .clone();
        // `document_mut` in a later stage records the record as it is now; the
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
        if withdrawn && admission {
            // Nothing of the admission is durable, so the invocation was
            // never admitted: an admission of it again is a new one, which
            // must reach the store, not an answer that it is already there.
            self.withdraw_admission(at);
            self.prune();
            return Ok(durable);
        }
        if withdrawn {
            self.roll_back(at, &started);
        }
        self.prune();
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
                    admission,
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
    /// Forgets the writes that have resolved, taking back first every
    /// admission among them that left nothing durable: one that resolved
    /// after its caller was answered is undone here, before the next
    /// admission could be told the invocation is already admitted.
    fn prune(&mut self) {
        let withdrawn: std::collections::BTreeSet<usize> = self
            .undurable
            .iter()
            .filter(|(_, undurable)| undurable.admission_withdrawn())
            .filter_map(|(invocation, _)| self.index.by_invocation.get(invocation).copied())
            .collect();
        // From the last position down, so each removal leaves the positions
        // still to remove where they were.
        for at in withdrawn.into_iter().rev() {
            self.withdraw_admission(at);
        }
        self.undurable.retain(|_, undurable| !undurable.settled());
    }

    /// Takes back record `at`, an admission no byte of which is durable.
    fn withdraw_admission(&mut self, at: usize) {
        if let Some(record) = self.snapshot.records.get(at) {
            self.undurable.remove(&record.key.invocation);
        }
        self.index.remove(&mut self.snapshot.records, at);
    }

    fn roll_back(&mut self, at: usize, started: &DurableInvocation) {
        if let Some(record) = self.snapshot.records.get_mut(at) {
            record.clone_from(started);
        }
    }

    /// `write` as durable here: the outcome, once it is waited for, with the
    /// record it made put back if the write left nothing durable.
    #[cfg(any(test, feature = "test-support"))]
    fn settle(
        &mut self,
        at: usize,
        started: &DurableInvocation,
        write: Durable,
    ) -> Result<(), String> {
        let outcome = write.wait_blocking();
        if writer::withdrew(&outcome) {
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
        #[cfg(any(test, feature = "test-support"))]
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
    #[cfg(any(test, feature = "test-support"))]
    pub fn barriers(&self) -> Barriers {
        self.counted.barriers()
    }

    /// Skip the barriers themselves, for tests that measure size.
    #[cfg(any(test, feature = "test-support"))]
    pub fn elide_barriers(&self) {
        self.counted.elide();
    }
}

/// The durable store as a snapshot value: what a restart replays before
/// recovery. It reads the file only; a live writer keeps its lock.
#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::expect_used, reason = "a test reads a store it just wrote")]
pub fn durable_snapshot(root: &Path) -> serde_json::Value {
    let mut file = File::open(root.join("records.json")).expect("invocation journal store");
    let loaded = Loaded::read(&mut file).expect("decodable invocation journal store");
    serde_json::to_value(&loaded.snapshot).expect("invocation journal snapshot value")
}

/// The digest of an admission intent's request, kept in place of the request.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Digest([u8; 32]);

impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Hex;
        impl Visitor<'_> for Hex {
            type Value = Digest;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("64 lowercase hex bytes")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Digest, E> {
                if s.len() != 64 {
                    return Err(E::custom("invalid digest length"));
                }
                if !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                    return Err(E::custom("noncanonical digest"));
                }
                let mut out = [0; 32];
                hex::decode_to_slice(s, &mut out).map_err(|_| E::custom("noncanonical digest"))?;
                Ok(Digest(out))
            }
        }
        d.deserialize_str(Hex)
    }
}

// Capture only update-level and public typed output/error duplicate fields.
// Arbitrary JSON values retain serde_json's existing map semantics.
pub struct RawUpdate {
    pub value: serde_json::Value,
    duplicate: Vec<String>,
    error_duplicate: Vec<String>,
    output_duplicate: Vec<String>,
}
struct Shallow {
    value: serde_json::Value,
    duplicate: Vec<String>,
}
impl<'de> Deserialize<'de> for Shallow {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Shallow;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Shallow, A::Error> {
                let mut out = serde_json::Map::new();
                let mut duplicate = vec![];
                while let Some(k) = m.next_key::<String>()? {
                    let v = m.next_value()?;
                    if out.contains_key(&k) {
                        duplicate.push(k.clone());
                    }
                    out.insert(k, v);
                }
                Ok(Shallow {
                    value: serde_json::Value::Object(out),
                    duplicate,
                })
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Shallow, A::Error> {
                let mut out = vec![];
                while let Some(v) = a.next_element()? {
                    out.push(v);
                }
                Ok(Shallow {
                    value: serde_json::Value::Array(out),
                    duplicate: vec![],
                })
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: s.into(),
                    duplicate: vec![],
                })
            }
            fn visit_string<E: serde::de::Error>(self, s: String) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: s.into(),
                    duplicate: vec![],
                })
            }
            fn visit_bool<E: serde::de::Error>(self, b: bool) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: b.into(),
                    duplicate: vec![],
                })
            }
            fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: n.into(),
                    duplicate: vec![],
                })
            }
            fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: n.into(),
                    duplicate: vec![],
                })
            }
            fn visit_f64<E: serde::de::Error>(self, n: f64) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: serde_json::Number::from_f64(n)
                        .map(serde_json::Value::Number)
                        .unwrap_or_default(),
                    duplicate: vec![],
                })
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: serde_json::Value::Null,
                    duplicate: vec![],
                })
            }
        }
        d.deserialize_any(V)
    }
}
impl<'de> Deserialize<'de> for RawUpdate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawUpdate;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("invocation update object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<RawUpdate, A::Error> {
                let mut out = serde_json::Map::new();
                let mut duplicate = vec![];
                let mut error_duplicate = vec![];
                let mut output_duplicate = vec![];
                while let Some(k) = m.next_key::<String>()? {
                    let v = if k == "error" || k == "output" {
                        let v = m.next_value::<Shallow>()?;
                        if k == "error" {
                            error_duplicate = v.duplicate;
                        } else {
                            output_duplicate = v.duplicate;
                        }
                        v.value
                    } else {
                        m.next_value()?
                    };
                    if out.contains_key(&k) {
                        duplicate.push(k.clone());
                    }
                    out.insert(k, v);
                }
                Ok(RawUpdate {
                    value: serde_json::Value::Object(out),
                    duplicate,
                    error_duplicate,
                    output_duplicate,
                })
            }
        }
        d.deserialize_map(V)
    }
}
impl RawUpdate {
    pub fn check_duplicates(&self) -> Result<(), String> {
        let state = self.value.get("state").and_then(serde_json::Value::as_str);
        let fields: &[&str] = match state {
            Some("accepted") => &["state", "invocation"],
            Some("progress") => &[
                "state",
                "invocation",
                "sequence",
                "phase",
                "completed",
                "total",
                "message",
            ],
            Some("item") => &["state", "invocation", "sequence", "value"],
            Some("succeeded") => &["state", "invocation", "output"],
            Some("failed") => &["state", "invocation", "error"],
            Some("cancelled") => &["state", "invocation", "message"],
            _ => &["state"],
        };
        if self
            .duplicate
            .iter()
            .any(|key| fields.contains(&key.as_str()))
            || (state == Some("failed")
                && self
                    .error_duplicate
                    .iter()
                    .any(|key| ["code", "message", "details", "retryable"].contains(&key.as_str())))
            || (state == Some("succeeded")
                && self
                    .output_duplicate
                    .iter()
                    .any(|key| ["value", "resources"].contains(&key.as_str())))
        {
            return Err("duplicate invocation update field".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    key: InvocationKey,
    intent: serde_json::Value,
    wire_id: u64,
    update: RawUpdate,
    uncertain: bool,
    cancellation_requested: bool,
    #[serde(default, deserialize_with = "prepared_present")]
    prepared_effect: Option<PreparedEffect>,
    #[serde(default, deserialize_with = "digest_present")]
    request_digest: Option<Digest>,
    #[serde(default)]
    admission: Option<crate::action_evidence::AdmissionEvidence>,
    #[serde(default)]
    terminal: Option<crate::action_evidence::TerminalReceipt>,
}
// Like the other optional members, absent only through #[serde(default)].
fn digest_present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Digest>, D::Error> {
    Digest::deserialize(d).map(Some)
}
fn prepared_present<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<PreparedEffect>, D::Error> {
    PreparedEffect::deserialize(d).map(Some)
}
impl<'de> Deserialize<'de> for DurableInvocation {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let row = Row::deserialize(d)?;
        row.update.check_duplicates().map_err(D::Error::custom)?;
        Ok(Self {
            key: row.key,
            intent: row.intent,
            request_digest: row.request_digest,
            wire_id: row.wire_id,
            update: serde_json::from_value(row.update.value).map_err(D::Error::custom)?,
            uncertain: row.uncertain,
            cancellation_requested: row.cancellation_requested,
            prepared_effect: row.prepared_effect,
            admission: row.admission,
            terminal: row.terminal,
        })
    }
}
impl Serialize for DurableInvocation {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut row = s.serialize_struct(
            "DurableInvocation",
            6_usize
                .saturating_add(usize::from(self.prepared_effect.is_some()))
                .saturating_add(usize::from(self.request_digest.is_some()))
                .saturating_add(usize::from(self.admission.is_some()))
                .saturating_add(usize::from(self.terminal.is_some())),
        )?;
        row.serialize_field("key", &self.key)?;
        row.serialize_field("intent", &self.intent)?;
        row.serialize_field("wire_id", &self.wire_id)?;
        row.serialize_field("update", &self.update)?;
        row.serialize_field("uncertain", &self.uncertain)?;
        row.serialize_field("cancellation_requested", &self.cancellation_requested)?;
        if let Some(prepared) = &self.prepared_effect {
            row.serialize_field("prepared_effect", prepared)?;
        }
        if let Some(digest) = &self.request_digest {
            row.serialize_field("request_digest", digest)?;
        }
        if let Some(admission) = &self.admission {
            row.serialize_field("admission", admission)?;
        }
        if let Some(terminal) = &self.terminal {
            row.serialize_field("terminal", terminal)?;
        }
        row.end()
    }
}

pub fn unknown(invocation: InvocationId) -> InvocationUpdate {
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
    use tilo_test_env::SpawnUnderEnvLock as _;

    const PRIVATE_DOCUMENT: &str = r#"{"version":7,"host":"00000000-0000-0000-0000-000000000063","records":[{"key":{"workspace":"00000000-0000-0000-0000-000000000001","host":"00000000-0000-0000-0000-000000000063","owner":{"Certified":{"root":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"device":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2]}},"invocation":"00000000-0000-0000-0000-000000000004"},"intent":{},"wire_id":1,"update":{"state":"succeeded","invocation":"00000000-0000-0000-0000-000000000004","output":{"value":"private result"}},"uncertain":false,"cancellation_requested":false}]}"#;

    tilo_test_env::scenario! {
        fn a_journal_store_opens_only_for_its_owner_in_a_shape_it_knows;

        phase a_store_whose_first_open_was_cut_short_is_initialized;
        // A first open cut short after the directory and its lock exist, but
        // before the first snapshot is written (the process was killed), left a
        // store that was never initialized. Opening it again initializes it;
        // refusing it left the desktop without collaboration on every start.
        {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("journal");
            drop(InvocationJournal::open(&root).unwrap());
            std::fs::remove_file(root.join("records.json")).unwrap();
            let mut journal = InvocationJournal::open(&root).expect("an uninitialized store opens");
            assert!(journal.snapshot.records.is_empty());
            let mut local = key();
            local.host = journal.host();
            local.owner = InvocationOwner::Local {
                actor: tilo_actions::Actor::from(tilo_actions::MemberId::person("person")),
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

        phase an_empty_store_an_earlier_tilo_wrote_is_replaced;
        // A computer an earlier Tilo set up kept a store of an older version with nothing in it:
        // it is replaced, where it stopped the daemon. One with invocations in it is still refused.
        {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("journal");
            drop(InvocationJournal::open(&root).unwrap());
            let earlier = r#"{"version":1,"host":"00000000-0000-0000-0000-000000000063","records":[]}"#;
            std::fs::write(root.join("records.json"), earlier).unwrap();
            let journal = InvocationJournal::open(&root).expect("an empty earlier store is replaced");
            assert!(journal.snapshot.records.is_empty());
            assert_eq!(journal.snapshot.version, VERSION);
            drop(journal);
            let kept: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("records.json")).unwrap()).unwrap();
            assert_eq!(kept["version"], VERSION, "the store on disk is the current one");

            let full = PRIVATE_DOCUMENT.replacen(r#""version":7"#, r#""version":1"#, 1);
            std::fs::write(root.join("records.json"), full).unwrap();
            assert!(
                InvocationJournal::open(&root).is_err(),
                "an earlier store with invocations in it is not dropped"
            );
        }

        phase a_private_store_reopens_with_its_owner;
        {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("journal");
            drop(InvocationJournal::open(&root).unwrap());
            std::fs::write(root.join("records.json"), PRIVATE_DOCUMENT).unwrap();
            let mut journal = InvocationJournal::open(&root).expect("private ownership reads");
            let before: serde_json::Value = serde_json::from_str(PRIVATE_DOCUMENT).unwrap();
            assert_eq!(serde_json::to_value(&journal.snapshot).unwrap(), before);
            let mut local = key();
            local.host = journal.host();
            local.owner = InvocationOwner::Local {
                actor: tilo_actions::Actor::from(tilo_actions::MemberId::person("person")),
            };
            journal.admit(local, serde_json::json!({}), 2).unwrap();
            assert_eq!(
                journal.snapshot.version, VERSION,
                "the store keeps its version"
            );
            drop(journal);
            let recovered = InvocationJournal::open(&root).unwrap();
            assert_eq!(
                serde_json::to_value(&recovered.snapshot.records[0]).unwrap(),
                before["records"][0]
            );
            let mut foreign = recovered.snapshot.records[0].key.clone();
            for owner in [
                InvocationOwner::Local {
                    actor: tilo_actions::Actor::from(tilo_actions::MemberId::person("person")),
                },
                InvocationOwner::Certified {
                    root: [1; 32],
                    device: [3; 32],
                },
                InvocationOwner::AuthenticatedLocal {
                    root: [1; 32],
                    device: [2; 32],
                    agent: None,
                },
                InvocationOwner::AuthenticatedLocal {
                    root: [1; 32],
                    device: [2; 32],
                    agent: Some("worker-a".into()),
                },
            ] {
                foreign.owner = owner;
                assert!(
                    recovered.lookup(&foreign).unwrap().is_none(),
                    "a recovered result answers its owner alone"
                );
            }
        }

        phase invocation_journal_rejects_preexisting_cross_owner_uuid_conflicts;
        {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("journal");
            let mut journal = InvocationJournal::open(&root).unwrap();
            let mut k = key();
            k.host = journal.host();
            journal.admit(k, serde_json::json!({}), 1).unwrap();
            let mut conflicting = journal.snapshot.records[0].clone();
            conflicting.key.owner = InvocationOwner::Local {
                actor: tilo_actions::Actor::from(tilo_actions::MemberId::person("other")),
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

        phase invocation_journal_lock_and_invalid_stores_fail_closed;
        {
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
    }

    tilo_test_env::scenario! {
        fn a_journal_restart_or_failed_barrier_never_reissues_an_admission;

        phase a_journal_barrier_never_returns_effect_permission_on_failure;
        {
            let private: Snapshot = serde_json::from_str(PRIVATE_DOCUMENT).expect("private owner data");
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
                    assert_eq!(reopened.snapshot.version, VERSION);
                    let retained = reopened.lookup(&key).unwrap().unwrap();
                    assert!(retained.uncertain);
                    assert_eq!(retained.key.owner, key.owner);
                } else {
                    assert_eq!(reopened.snapshot.version, VERSION);
                    assert!(reopened.lookup(&key).unwrap().is_none());
                }
            }
        }

        phase invocation_journal_initialization_parent_barrier_must_succeed_on_reopen;
        {
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

        phase invocation_journal_restart_retains_terminal_and_never_reissues_admission;
        {
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

        phase invocation_journal_recovery_of_admission_is_uncertain_and_private;
        {
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

        phase invocation_journal_directory_barrier_failure_never_reopens_identity;
        {
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

    /// An admission whose write the disk takes back after its caller was
    /// answered was never admitted: the caller's retry is a new admission
    /// that reaches the store, not an answer that it is already there.
    #[test]
    fn a_retry_of_an_admission_the_disk_took_back_late_is_admitted_again() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let mut journal = InvocationJournal::open(&root).unwrap();
        let mut invocation = key();
        invocation.host = journal.host();
        // The writer holds the admission's write until the admission has
        // answered, as a loaded disk would.
        let (release, resume) = std::sync::mpsc::sync_channel::<()>(1);
        let resume = std::sync::Mutex::new(resume);
        journal.hold_writes = Some(std::sync::Arc::new(move || {
            let _ = resume.lock().unwrap().recv();
        }));
        journal.fail_at = Some(BarrierFailure::File);
        let (fresh, durable) = journal
            .admit_with_evidence(invocation.clone(), serde_json::json!({}), 1, None)
            .unwrap();
        assert!(fresh);
        assert!(
            !durable.is_settled(),
            "the admission is still being written"
        );
        release.send(()).unwrap();
        assert!(durable.wait_blocking().is_err());
        assert!(
            journal.lookup(&invocation).unwrap().is_none(),
            "an admission the disk took back is not read as admitted"
        );

        journal.fail_at = None;
        journal.hold_writes = None;
        let (fresh, durable) = journal
            .admit_with_evidence(invocation.clone(), serde_json::json!({}), 1, None)
            .unwrap();
        assert!(
            fresh,
            "the retry admits the invocation the store never kept"
        );
        durable.wait_blocking().unwrap();
        assert!(journal.lookup(&invocation).unwrap().is_some());
        drop(journal);
        assert!(
            InvocationJournal::open(&root)
                .unwrap()
                .lookup(&invocation)
                .unwrap()
                .is_some(),
            "the retried admission is in the store"
        );
    }

    fn key() -> InvocationKey {
        InvocationKey {
            workspace: "5ca720b0-7649-4bdc-9c30-77380f7c2be5".into(),
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
    fn invocation_journal_excludes_a_second_process() {
        const PROBE: &str = "TILO_INVOCATION_JOURNAL_LOCK_PROBE";
        if let Some(root) = std::env::var_os(PROBE) {
            assert!(InvocationJournal::open(Path::new(&root)).is_err());
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("journal");
        let _journal = InvocationJournal::open(&root).unwrap();
        // By the name alone: the harness names the case by its whole module
        // path, which `--exact` would have to spell out and this test only
        // knows as a suffix.
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("invocation_journal_excludes_a_second_process")
            .env(PROBE, &root)
            .output_locked()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
    }
}

#[cfg(test)]
mod authenticated_local_tests;

#[cfg(test)]
mod append_tests;
