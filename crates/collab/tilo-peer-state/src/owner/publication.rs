//! Same-barrier native outgoing ownership; server queue acceptance is never an ACK.
use super::answers::Unanswered;
use super::backlog::Ownership;
use crate::StatePeer;
use crate::owner::{NativeCommittedView, NativeWorkspaceOwner, WorkspaceId, enrollment, stored};
use crate::owner_commit::{CommitBinding, CommitOperation, OwnerCommitRequest};
use collab_types::signer::DeviceSigner;
use collab_types::{OpKind, SignedOp};
use neighbour_wire::NeighbourMsg;
use neighbour_wire::Retention;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use web_time::SystemTime;

mod history_keys;

pub use history_keys::SealedHistoryKeys;
use history_keys::seal_for_document;

/// Writes ownership keeps in its record at its head, where the server session sends them from,
/// and at most as many again at its tail. Every write past them waits in runs of the store's
/// beside the record ([`super::Store::put_ownership`]): ownership takes every write this device
/// makes, linked or not, however long its host is away. This crate's tests keep fewer, so a
/// test crosses the bound many times over in the writes it can afford.
#[cfg(not(test))]
pub(crate) const MAX_ENTRIES: usize = 64;
#[cfg(test)]
pub(crate) const MAX_ENTRIES: usize = 8;
pub(crate) const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Bytes of writes one batch of ownership's backlog gathers before the store keeps it.
const RUN_BYTES: usize = 1024 * 1024;
/// Most bytes a batch the store keeps can hold: its writes, then one more that did not fit.
pub(crate) const MAX_RUN_BYTES: usize = RUN_BYTES + MAX_BYTES + 64 * 1024;
pub(crate) const MAX_VISIBLE_ENTRIES: usize = MAX_ENTRIES + 1;
/// Passes (writes, and asks of the workspace's holder) before a compaction that
/// drafted nothing is tried again ([`NativeWorkspaceOwner::compact_outgoing`]).
const COMPACTION_RETRY: usize = 256;

/// The records whose compaction drafted nothing, each resting until its passes are spent.
///
/// The rest is the record's own, not the workspace's: one history that cannot fold yet — its
/// keys are not here, or it does not merge — must not keep the workspace's other histories from
/// folding, which leaves them to fill and refuse writes.
#[derive(Default)]
pub(crate) struct Resting(
    std::sync::Mutex<std::collections::BTreeMap<(WorkspaceId, String, String), usize>>,
);

impl Resting {
    /// Count one pass over `workspace`, and answer the records that still rest.
    fn pass(&self, workspace: WorkspaceId) -> std::collections::BTreeSet<(String, String)> {
        let mut resting = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut still = std::collections::BTreeSet::new();
        resting.retain(|(resting_workspace, app, record), wait| {
            if *resting_workspace != workspace {
                return true;
            }
            *wait = wait.saturating_sub(1);
            if *wait == 0 {
                return false;
            }
            still.insert((app.clone(), record.clone()));
            true
        });
        still
    }

    /// `record` of `workspace` drafted nothing: it rests a while.
    fn failed(&self, workspace: WorkspaceId, (app, record): (String, String)) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((workspace, app, record), COMPACTION_RETRY);
    }

    /// `record` of `workspace` folded: it is tried again when it is next due.
    fn folded(&self, workspace: WorkspaceId, (app, record): (String, String)) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(workspace, app, record));
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredPublication {
    version: u32,
    revision: u64,
    next_ordinal: u64,
    #[serde(deserialize_with = "read_entries")]
    pub(crate) entries: Vec<StoredOutgoing>,
    /// Recovery projections an earlier build signed for its expiring
    /// frames. This build re-sends the signed originals instead, so the list
    /// is read and then dropped (`file_store::read_resource`); it stays in the
    /// format so records that hold one still load.
    #[serde(
        default,
        deserialize_with = "read_entries",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub(crate) projection: Vec<StoredOutgoing>,
    #[serde(default, deserialize_with = "read_entries")]
    governance: Vec<StoredOutgoing>,
    /// What the workspace's server answered to this device's writes, per document: a map whose
    /// copies share what neither changed, so an answer journals that document's entry and not
    /// every document's ([`crate::owner::journal`]).
    #[serde(
        default,
        skip_serializing_if = "Answered::is_empty",
        with = "answered_list"
    )]
    answered: Answered,
    /// Writes behind `entries` the store keeps beside the record, oldest first: they move to
    /// the head as it sends and the host answers what it holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    spilled: Vec<SpilledRun>,
    /// The newest writes behind `spilled`, until they make a run of their own.
    #[serde(
        default,
        deserialize_with = "read_entries",
        skip_serializing_if = "Vec::is_empty"
    )]
    tail: Vec<StoredOutgoing>,
    /// The sealed records whose history here a compaction of this device's own folded ahead
    /// of the host's, each with the writes the host holds that it no longer holds as
    /// themselves: what this device knows of the host's history beside its own. All but its
    /// newest writes wait in runs the store keeps, so an answer never rewrites them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hosted: Vec<HostedHistory>,
    /// Ownership's compactions not drafted yet. Each keeps its place, and the sequence it was
    /// committed under, until it is the next write ownership sends: it is drafted then, against
    /// the history its host acknowledged ([`NativeWorkspaceOwner::compact_outgoing`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    slots: Vec<[u8; 32]>,
    /// This device's writes its server has not taken, by signature ([`super::answers`]): a map
    /// whose copies share what neither changed, so an answer journals the writes it took and not
    /// every write still waiting.
    #[serde(
        default,
        skip_serializing_if = "Unanswered::is_empty",
        with = "super::answers::unanswered_list"
    )]
    pub(super) unanswered: Unanswered,
}
/// One of [`StoredPublication::hosted`]: the host's writes of `record` of `app` the record
/// holds here only inside this device's compaction, in the host's order: those of `runs`
/// ([`super::Store::put_ownership`]), then `writes`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HostedHistory {
    app: String,
    record: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    runs: Vec<[u8; 32]>,
    writes: Vec<HostedWrite>,
    /// Each writer's highest write a compaction of the host's folded: the host holds none
    /// of them as themselves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    covered: Vec<([u8; 32], u64)>,
}
/// What this device knows of the host's history of a record whose history here a compaction of
/// its own folded ahead of the host's ([`StoredPublication::hosted`]).
#[derive(Clone, Debug, Default)]
struct Hosted {
    /// The host's writes the record holds here only inside that compaction, in its order.
    writes: Vec<SignedOp>,
    covered: BTreeMap<[u8; 32], u64>,
}
impl Hosted {
    /// Whether a compaction of the host's folded `write`.
    fn covers(&self, write: &SignedOp) -> bool {
        self.covered
            .get(write.author.as_bytes())
            .is_some_and(|seq| write.seq <= *seq)
    }
    /// Take `compaction`, now first in the host's history.
    fn take(&mut self, compaction: &SignedOp) -> Result<(), String> {
        let wrapper =
            collab_types::SealedCompaction::parse(compaction).map_err(|e| e.to_string())?;
        for (author, seq) in &wrapper.covers {
            let floor = self.covered.entry(*author.as_bytes()).or_default();
            *floor = (*floor).max(*seq);
        }
        let writes = std::mem::take(&mut self.writes);
        self.writes = std::iter::once(compaction.clone())
            .chain(
                writes
                    .into_iter()
                    .filter(|write| write.signature != compaction.signature && !self.covers(write)),
            )
            .collect();
        Ok(())
    }
}
/// A write of a [`HostedHistory`], as postcard bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HostedWrite(#[serde(with = "crate::owner::stored::base64_bytes")] Vec<u8>);
impl HostedHistory {
    /// Keep all but the newest writes in runs, as ownership keeps its backlog.
    fn spill(&mut self, (store, workspace, _): Ownership<'_>) -> Result<(), String> {
        let kept = spill(&mut self.writes, |write| write.0.len(), store, workspace)?;
        self.runs.extend(kept.into_iter().map(|(hash, _)| hash));
        Ok(())
    }
}
/// How many writes of what this device knows of its hosts' histories `view`'s record holds
/// itself, and how many runs beside it hold the rest.
#[cfg(test)]
pub(crate) fn hosted_in_document(view: &NativeCommittedView) -> (usize, usize) {
    state(view).map_or((0, 0), |publication| {
        publication
            .hosted
            .iter()
            .fold((0, 0), |(writes, runs), held| {
                (
                    writes.saturating_add(held.writes.len()),
                    runs.saturating_add(held.runs.len()),
                )
            })
    })
}
/// Writes of ownership's backlog the store keeps as one run, named by the hash of its bytes
/// ([`super::Store::put_ownership`]), and how many of them moved to ownership's head already.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SpilledRun {
    pub(crate) hash: [u8; 32],
    pub(crate) writes: u32,
    pub(crate) taken: u32,
}
/// The server host's answers to this device's writes to one record, kept with the ownership they
/// left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Answer {
    durable: u64,
    refused: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_order: Option<u64>,
}

impl Answer {
    /// Its writes kept, its writes refused and the order of the last one kept.
    pub(crate) const fn parts(self) -> (u64, u64, Option<u64>) {
        (self.durable, self.refused, self.last_order)
    }

    /// The answer [`Self::parts`] names.
    pub(crate) const fn from_parts(durable: u64, refused: u64, last_order: Option<u64>) -> Self {
        Self {
            durable,
            refused,
            last_order,
        }
    }
}

/// The server's answers, by app and document.
pub(crate) type Answered = crate::ordered::Ordered<(String, String), Answer>;

/// [`Answered`] as the list the workspace's file holds.
mod answered_list {
    use super::{Answer, Answered};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct AnsweredDocument {
        app: String,
        record: String,
        durable: u64,
        refused: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_order: Option<u64>,
    }

    pub(super) fn serialize<S: Serializer>(
        answered: &Answered,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(
            answered
                .iter()
                .map(|((app, document), answer)| AnsweredDocument {
                    app: app.clone(),
                    record: document.clone(),
                    durable: answer.durable,
                    refused: answer.refused,
                    last_order: answer.last_order,
                }),
        )
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Answered, D::Error> {
        Ok(Vec::<AnsweredDocument>::deserialize(deserializer)?
            .into_iter()
            .map(|held| {
                let answer = Answer {
                    durable: held.durable,
                    refused: held.refused,
                    last_order: held.last_order,
                };
                ((held.app, held.record), answer)
            })
            .collect())
    }
}
/// What a workspace's server host answered to one of this device's writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAnswer {
    /// Kept durably, at this place in the host's order.
    Durable(u64),
    /// Refused: it never becomes durable.
    Refused,
}
/// This device's writes to one record: those still in its ownership, and what the workspace's
/// server host answered to the others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocumentWrites {
    pub pending: usize,
    pub durable: u64,
    pub refused: u64,
    /// The host's order of the latest write it kept.
    pub last_order: Option<u64>,
}
impl Default for StoredPublication {
    fn default() -> Self {
        Self {
            version: 3,
            revision: 1,
            next_ordinal: 1,
            entries: Vec::new(),
            projection: Vec::new(),
            governance: Vec::new(),
            answered: Answered::default(),
            spilled: Vec::new(),
            tail: Vec::new(),
            hosted: Vec::new(),
            slots: Vec::new(),
            unanswered: Unanswered::default(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredOutgoing {
    ordinal: u64,
    pub(crate) id: [u8; 32],
    /// Base64 text in the owner's JSON files, like the record's other
    /// opaque bytes; a binary encoding keeps its sequence.
    #[serde(
        serialize_with = "crate::owner::stored::base64_bytes::serialize",
        deserialize_with = "read_message_bytes"
    )]
    pub(crate) bytes: Vec<u8>,
    binding: CommitBinding,
    timestamp_ns: u64,
    channel: [u8; 32],
    incremental: bool,
}
#[cfg(test)]
impl StoredOutgoing {
    /// A write of `bytes` named by `id` in each of its bytes.
    pub(crate) fn sample(id: u8, bytes: Vec<u8>) -> Self {
        Self {
            ordinal: 3,
            id: [id; 32],
            bytes,
            binding: CommitBinding {
                workspace: "workspace".into(),
                target: crate::owner_commit::CommitTarget::Document {
                    app: "so.tilo.code".into(),
                    record: "a".into(),
                },
                request_digest: [1; 32],
                basis: [2; 32],
            },
            timestamp_ns: 5,
            channel: [9; 32],
            incremental: true,
        }
    }
}
pub struct NativeOutgoing<'a> {
    state: &'a StoredPublication,
    allowed: [bool; MAX_VISIBLE_ENTRIES],
}
pub struct NativeOutgoingEntry<'a>(&'a StoredOutgoing);
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeOutgoingStatus {
    pub pending: usize,
    pub blocked: usize,
}
impl<'a> NativeOutgoing<'a> {
    /// Every entry still held, whether or not it may be sent now.
    pub fn queued(&self) -> impl Iterator<Item = [u8; 32]> + 'a {
        self.state
            .governance
            .iter()
            .chain(self.state.entries.iter())
            .map(|entry| entry.id)
    }
    /// Every entry still held, whether or not it may be sent now: what ownership answers for,
    /// which a write the workspace's rules no longer admit here still is.
    pub fn held(&self) -> impl Iterator<Item = NativeOutgoingEntry<'a>> + 'a {
        self.state
            .governance
            .iter()
            .chain(self.state.entries.iter())
            .map(NativeOutgoingEntry)
    }
    pub fn entries(&self) -> impl Iterator<Item = NativeOutgoingEntry<'a>> {
        let allowed = self.allowed;
        self.state
            .governance
            .iter()
            .chain(self.state.entries.iter())
            .enumerate()
            .filter(move |(index, _)| allowed.get(*index).copied().unwrap_or(false))
            .map(|(_, entry)| NativeOutgoingEntry(entry))
    }
}
impl<'a> NativeOutgoingEntry<'a> {
    #[must_use]
    pub fn id(&self) -> [u8; 32] {
        self.0.id
    }
    #[must_use]
    pub fn bytes(&self) -> &'a [u8] {
        &self.0.bytes
    }
    #[must_use]
    pub fn channel(&self) -> [u8; 32] {
        self.0.channel
    }
    /// How the store keeps this entry: a write appends after the record's
    /// checkpoint, while a checkpoint of this author's own — the workspace's
    /// governance snapshot or its sealed compaction of a record — stands as
    /// the whole of what it retained there, so the store drops the frames it
    /// covers.
    #[must_use]
    pub fn retention(&self) -> Retention {
        if self.0.incremental {
            Retention::Delta
        } else {
            Retention::Checkpoint
        }
    }
}
fn read_entries<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> Result<Vec<StoredOutgoing>, D::Error> {
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = Vec<StoredOutgoing>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded native publication entries")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut input: A,
        ) -> Result<Self::Value, A::Error> {
            if input.size_hint().is_some_and(|size| size > MAX_ENTRIES) {
                return Err(serde::de::Error::custom("native outgoing entry capacity"));
            }
            let mut entries = Vec::new();
            let mut bytes = 0usize;
            loop {
                if entries.len() == MAX_ENTRIES {
                    if input.next_element::<serde::de::IgnoredAny>()?.is_some() {
                        return Err(serde::de::Error::custom("native outgoing entry capacity"));
                    }
                    return Ok(entries);
                }
                let Some(entry) = input.next_element::<StoredOutgoing>()? else {
                    return Ok(entries);
                };
                bytes = bytes
                    .checked_add(entry.bytes.len())
                    .filter(|bytes| *bytes <= MAX_BYTES)
                    .ok_or_else(|| serde::de::Error::custom("native outgoing byte capacity"))?;
                entries.push(entry);
            }
        }
    }
    input.deserialize_seq(Entries)
}
fn read_bytes<'de, D: serde::Deserializer<'de>>(
    input: D,
    limit: usize,
) -> Result<Vec<u8>, D::Error> {
    struct Bytes(usize);
    impl<'de> serde::de::Visitor<'de> for Bytes {
        type Value = Vec<u8>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded native publication bytes")
        }
        fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
            use base64::{Engine as _, engine::general_purpose::STANDARD};
            // Refuse oversized text before decoding any of it.
            if text.len() > self.0.div_ceil(3).saturating_mul(4) {
                return Err(E::custom("native publication bytes exceed limit"));
            }
            let bytes = STANDARD.decode(text).map_err(E::custom)?;
            if bytes.len() > self.0 {
                return Err(E::custom("native publication bytes exceed limit"));
            }
            Ok(bytes)
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut input: A,
        ) -> Result<Self::Value, A::Error> {
            if input.size_hint().is_some_and(|size| size > self.0) {
                return Err(serde::de::Error::custom(
                    "native publication bytes exceed limit",
                ));
            }
            let mut bytes = Vec::new();
            while let Some(byte) = input.next_element::<u8>()? {
                if bytes.len() == self.0 {
                    return Err(serde::de::Error::custom(
                        "native publication bytes exceed limit",
                    ));
                }
                bytes.push(byte);
            }
            Ok(bytes)
        }
    }
    // Base64 text in JSON; the unchanged sequence in a binary encoding,
    // which writes one.
    if input.is_human_readable() {
        input.deserialize_str(Bytes(limit))
    } else {
        input.deserialize_seq(Bytes(limit))
    }
}
fn read_message_bytes<'de, D: serde::Deserializer<'de>>(input: D) -> Result<Vec<u8>, D::Error> {
    read_bytes(input, MAX_BYTES)
}
fn fresh(entry: &StoredOutgoing, now: SystemTime) -> bool {
    let Some(timestamp) =
        SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_nanos(entry.timestamp_ns))
    else {
        return false;
    };
    let skew = std::time::Duration::from_secs(300);
    timestamp >= now.checked_sub(skew).unwrap_or(SystemTime::UNIX_EPOCH)
        && timestamp <= now.checked_add(skew).unwrap_or(now)
}
pub(crate) fn state(view: &NativeCommittedView) -> Result<&StoredPublication, String> {
    state_of(&view.record)
}

/// The outgoing ownership of a record, published or in the making.
pub(crate) fn state_of(record: &stored::StoredWorkspace) -> Result<&StoredPublication, String> {
    record
        .publication
        .as_ref()
        .ok_or_else(|| "native publication state is missing".into())
}
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn status(
    view: &NativeCommittedView,
    now: SystemTime,
) -> Result<NativeOutgoingStatus, String> {
    let publication = state(view)?;
    let allowed = allowed(view, now, &mut BTreeSet::new())?;
    // `bounds` caps both lists, so neither sum nor difference can wrap.
    let total = publication
        .governance
        .len()
        .saturating_add(publication.entries.len());
    Ok(NativeOutgoingStatus {
        pending: total,
        blocked: total.saturating_sub(allowed.iter().filter(|allowed| **allowed).count()),
    })
}
/// Which of ownership's entries may go now, `verified` holding the writes earlier passes checked:
/// a write's bytes never change, so it is checked once and added. Governance entries are decided
/// against the workspace as it stands, which their check reads.
fn allowed(
    view: &NativeCommittedView,
    now: SystemTime,
    verified: &mut BTreeSet<[u8; 32]>,
) -> Result<[bool; MAX_VISIBLE_ENTRIES], String> {
    let publication = state(view)?;
    bounds(publication)?;
    let mut allowed = [false; MAX_VISIBLE_ENTRIES];
    for (index, entry) in publication.governance.iter().enumerate() {
        if let Some(slot) = allowed.get_mut(index) {
            *slot = validate_governance(view, entry).is_ok();
        }
    }
    let governance_len = publication.governance.len();
    // The records with a compaction ahead held: none of their writes leaves before the host
    // answered it, so one it refuses is drafted again in its place.
    let mut compacting: Vec<(String, String)> = Vec::new();
    for (index, entry) in publication.entries.iter().enumerate() {
        // A compaction not drafted yet holds its place: nothing after it leaves before it.
        if publication.slots.contains(&entry.id) {
            tracing::debug!(
                held = publication.entries.len().saturating_sub(index),
                "this device's writes wait held behind a compaction not drafted yet"
            );
            break;
        }
        let message: NeighbourMsg =
            NeighbourMsg::decode(&entry.bytes).map_err(|e| e.to_string())?;
        let NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return Err("invalid native outgoing document".into());
        };
        let route = (app_id, document_id);
        if compacting.contains(&route) {
            tracing::debug!(
                app = %route.0,
                record = %route.1,
                "a write waits held behind its record's compaction the host has not answered"
            );
            continue;
        }
        if envelope.kind == OpKind::Compaction {
            compacting.push(route.clone());
        }
        let (app_id, document_id) = route;
        // A sealed write expires with the channel epoch it was made under:
        // its key holder renews it ([`NativeWorkspaceOwner::renew_outgoing`]),
        // and the server never re-sends the expired original beside a renewal.
        // A cleartext frame is re-sent as it was signed, however old it is.
        if envelope.encryption.is_some() && !fresh(entry, now) {
            tracing::debug!(
                app = %app_id,
                record = %document_id,
                "a sealed write held is past its epoch and waits for its renewal"
            );
            continue;
        }
        if let Some(place) = allowed.get_mut(governance_len.saturating_add(index)) {
            let admitted = verified.contains(&entry.id)
                || (envelope.author.as_bytes() == &view.device().device_public_key
                    && view
                        .peer
                        .lookup_exact_local_document(&app_id, &document_id, &envelope, now)
                        .ok()
                        .flatten()
                        .is_some());
            *place = admitted;
            if admitted {
                verified.insert(entry.id);
            }
            if !admitted {
                tracing::debug!(
                    app = %app_id,
                    record = %document_id,
                    "a write held is not this device's as its replica holds it, and stays"
                );
            }
        }
    }
    Ok(allowed)
}
/// Whether a host file of layout `version` may carry `storage`. Workspace
/// records came in with layout 4; later layouts carry them as it does.
pub fn validate_file_layout(
    version: u32,
    storage: &stored::NativeAuthorityStorage,
) -> Result<(), String> {
    for workspace in storage.workspaces.values() {
        match (version, workspace.version, workspace.publication.is_some()) {
            (4.., stored::WORKSPACE_VERSION, true) => {}
            _ => return Err("native publication record/file layout mismatch".into()),
        }
        if let Some(publication) = &workspace.publication {
            bounds(publication)?;
        }
    }
    Ok(())
}
fn bounds(publication: &StoredPublication) -> Result<usize, String> {
    if !matches!(publication.version, 2 | 3)
        || (publication.version == 2 && !publication.governance.is_empty())
        || publication.revision == 0
        || publication.next_ordinal == 0
        || publication.entries.len() > MAX_ENTRIES
        || publication.governance.len() > 1
        || publication.spilled.iter().any(|run| {
            run.writes == 0
                || run.taken >= run.writes
                || usize::try_from(run.writes).map_or(true, |writes| writes > MAX_ENTRIES)
        })
    {
        return Err("invalid native outgoing publication or entry capacity".into());
    }
    let bytes = publication
        .governance
        .iter()
        .chain(publication.entries.iter())
        .try_fold(0usize, |bytes, entry| {
            bytes
                .checked_add(entry.bytes.len())
                .ok_or("native outgoing byte overflow")
        })?;
    if bytes > MAX_BYTES {
        return Err("native outgoing byte capacity exceeded".into());
    }
    Ok(bytes)
}
pub(crate) fn request(message: &NeighbourMsg) -> Result<OwnerCommitRequest, String> {
    let NeighbourMsg::AppDocument {
        app_id,
        cluster_id,
        document_id,
        envelope,
        readers,
        ..
    } = message
    else {
        return Err("native outgoing entry must be an exact document".into());
    };
    Ok(OwnerCommitRequest {
        workspace: cluster_id.clone(),
        operation: CommitOperation::Document {
            app: app_id.clone(),
            document: document_id.clone(),
            envelope: envelope.clone(),
            readers: readers.clone(),
        },
    })
}
pub(crate) fn metadata(message: &NeighbourMsg) -> Result<(u64, [u8; 32], bool), String> {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        return Err("native outgoing requires document".into());
    };
    // The one host classifier (`neighbour_wire::retention`), shared with
    // browser peers and the server. A single-use MLS application message has
    // no retention class and is refused here like any live frame.
    let (channel, kind) = neighbour_wire::retention(message, false)
        .ok_or("unsupported native outgoing document payload")?;
    Ok((
        enrollment::ns(envelope.timestamp)?,
        channel,
        member_publishes_as_delta(kind, envelope.encryption.is_some(), envelope.kind),
    ))
}
/// Whether a member publishes this document as a retained delta.
///
/// A cleartext snapshot keeps the checkpoint class the classifier gives it:
/// it is the record's whole state, and the class tells the store it stands as
/// the record's checkpoint rather than one more change after it.
///
/// A content-key ciphertext the classifier calls a checkpoint is published
/// as a delta instead, unless it is a compaction: the host then appends it
/// after the record's checkpoint rather than letting a member's ordinary
/// write replace it. A sealed compaction is the one member checkpoint that
/// leaves as itself, because it is the one that covers the sealed history
/// before it, which every store then drops.
fn member_publishes_as_delta(kind: Retention, sealed: bool, op: OpKind) -> bool {
    match kind {
        Retention::Delta => true,
        // A cleartext snapshot is the governor's baseline, whoever writes it.
        Retention::Checkpoint if !sealed => false,
        Retention::Checkpoint => op != OpKind::Compaction,
    }
}
struct Count {
    bytes: usize,
    limit: usize,
}
impl postcard::ser_flavors::Flavor for Count {
    type Output = usize;
    fn try_push(&mut self, _: u8) -> postcard::Result<()> {
        self.try_extend(&[0])
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|bytes| *bytes <= self.limit)
            .ok_or(postcard::Error::SerializeBufferFull)?;
        Ok(())
    }
    fn finalize(self) -> postcard::Result<usize> {
        Ok(self.bytes)
    }
}
pub(crate) fn encode(message: &NeighbourMsg, limit: usize) -> Result<Vec<u8>, String> {
    postcard::serialize_with_flavor(message, Count { bytes: 0, limit })
        .map_err(|e| e.to_string())?;
    postcard::to_allocvec(message).map_err(|e| e.to_string())
}
/// The bytes ownership keeps, and its server session sends, for `request`, one of this device's
/// document writes as the owner committed it.
///
/// # Errors
/// `request` is not a document write, or it exceeds ownership's cap.
pub fn outgoing_bytes(request: &OwnerCommitRequest) -> Result<Vec<u8>, String> {
    prepare(request)
}
pub(crate) fn prepare(request: &OwnerCommitRequest) -> Result<Vec<u8>, String> {
    let CommitOperation::Document {
        app,
        document: record,
        envelope,
        readers,
    } = &request.operation
    else {
        return Err("native outgoing requires document".into());
    };
    // This is the same exact signed local envelope, with canonical transport routing.
    postcard::serialize_with_flavor(
        request,
        Count {
            bytes: 0,
            limit: MAX_BYTES,
        },
    )
    .map_err(|e| e.to_string())?;
    let peer = crate::peer_handle(envelope.author.as_bytes());
    encode(
        &NeighbourMsg::AppDocument {
            peer,
            app_id: app.clone(),
            cluster_id: request.workspace.clone(),
            document_id: record.clone(),
            envelope: envelope.clone(),
            readers: readers.clone(),
        },
        MAX_BYTES,
    )
}
pub(crate) fn replace_governance_checkpoint(
    live: &StatePeer,
    view: &mut stored::StoredWorkspace,
    binding: CommitBinding,
    now: SystemTime,
) -> Result<(), String> {
    let message = live.workspace_message();
    let bytes = encode(&message, MAX_BYTES)?;
    let existing = state_of(view)?.governance.first();
    if existing.is_some_and(|entry| entry.bytes == bytes && entry.binding == binding) {
        return Ok(());
    }
    let total = bounds(state_of(view)?)?;
    let old = existing.map_or(0, |entry| entry.bytes.len());
    if total
        .checked_sub(old)
        .and_then(|size| size.checked_add(bytes.len()))
        .is_none_or(|size| size > MAX_BYTES)
    {
        return Err("native governance checkpoint exceeds outgoing capacity".into());
    }
    let (channel, retention) = neighbour_wire::retention(&message, true)
        .ok_or("unsupported native governance checkpoint")?;
    if retention != Retention::Checkpoint {
        return Err("native governance publication is not a checkpoint".into());
    }
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    publication.version = 3;
    let ordinal = publication.next_ordinal;
    publication.next_ordinal = ordinal
        .checked_add(1)
        .ok_or("native publication ordinal exhausted")?;
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    publication.governance = vec![StoredOutgoing {
        ordinal,
        id: *blake3::hash(&bytes).as_bytes(),
        bytes,
        binding,
        timestamp_ns: enrollment::ns(now)?,
        channel,
        incremental: false,
    }];
    Ok(())
}

/// Publish `changes`, workspace changes this member's device made itself, for
/// the workspace's host to admit as it admits every member's: one retained
/// delta, joined with the changes still waiting from before, which stays
/// until the workspace's governance carries them all
/// ([`retire_member_changes`]).
pub(crate) fn add_member_changes(
    live: &StatePeer,
    view: &mut stored::StoredWorkspace,
    changes: Vec<Vec<u8>>,
    binding: CommitBinding,
    now: SystemTime,
) -> Result<(), String> {
    let mut joined = match state_of(view)?.governance.first() {
        Some(existing) if existing.incremental => member_changes(&existing.bytes)?,
        _ => Vec::new(),
    };
    for change in changes {
        if !joined.contains(&change) {
            joined.push(change);
        }
    }
    // A change names the changes it was written on. The rows this device
    // wrote between two membership changes are its own and reach no other
    // device but here: a delta that leaves them out names changes its
    // receivers do not hold, and they hold the whole delta back. It carries
    // every change this device made that the entry does not.
    let own = live.workspace_actor_for(&view.device.device_public_key);
    for bytes in live.workspace_changes_since(&[]) {
        let change =
            automerge::Change::try_from(bytes.as_slice()).map_err(|error| error.to_string())?;
        if change.actor_id() == &own && !joined.contains(&bytes) {
            joined.push(bytes);
        }
    }
    let message = NeighbourMsg::SyncResp {
        changes: joined,
        workspace: view.workspace.to_string(),
    };
    let bytes = encode(&message, MAX_BYTES)?;
    let old = state_of(view)?
        .governance
        .first()
        .map_or(0, |entry| entry.bytes.len());
    let total = bounds(state_of(view)?)?;
    if total
        .checked_sub(old)
        .and_then(|size| size.checked_add(bytes.len()))
        .is_none_or(|size| size > MAX_BYTES)
    {
        return Err("membership changes exceed outgoing capacity".into());
    }
    let (channel, retention) = neighbour_wire::retention(&message, false)
        .ok_or("unsupported native membership publication")?;
    if retention != Retention::Delta {
        return Err("native membership publication is not a delta".into());
    }
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    publication.version = 3;
    let ordinal = publication.next_ordinal;
    publication.next_ordinal = ordinal
        .checked_add(1)
        .ok_or("native publication ordinal exhausted")?;
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    publication.governance = vec![StoredOutgoing {
        ordinal,
        id: *blake3::hash(&bytes).as_bytes(),
        bytes,
        binding,
        timestamp_ns: enrollment::ns(now)?,
        channel,
        incremental: true,
    }];
    Ok(())
}

/// The changes a member's published workspace delta carries.
fn member_changes(bytes: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    match NeighbourMsg::decode(bytes).map_err(|e| e.to_string())? {
        NeighbourMsg::SyncResp { changes, .. } => Ok(changes),
        _ => Err("native membership publication is not a workspace delta".into()),
    }
}

/// The hashes of `changes`, encoded workspace changes.
pub(crate) fn change_hashes(
    changes: &[Vec<u8>],
) -> Result<BTreeSet<automerge::ChangeHash>, String> {
    changes
        .iter()
        .map(|bytes| {
            automerge::Change::from_bytes(bytes.clone())
                .map(|change| change.hash())
                .map_err(|e| e.to_string())
        })
        .collect()
}

/// Let go of this member's published workspace changes once the workspace's
/// governance, as one of the owner's devices sent it (`governed`), carries
/// every one of them: the host admitted them. A workspace its governor started
/// over from a newer base no longer holds them as they were made: that base
/// is what the workspace is now, and they go too.
///
/// `governed` is the changes as each delta carried them, a delta whose changes
/// do not all decode carrying none: they are decoded only when this member has
/// changes of its own waiting.
pub(crate) fn retire_member_changes(
    live: &StatePeer,
    view: &mut stored::StoredWorkspace,
    governed: &[Vec<Vec<u8>>],
) -> Result<(), String> {
    let Some(existing) = state_of(view)?.governance.first() else {
        return Ok(());
    };
    if !existing.incremental {
        return Ok(());
    }
    let made = change_hashes(&member_changes(&existing.bytes)?)?;
    let governed: BTreeSet<automerge::ChangeHash> = governed
        .iter()
        .filter_map(|changes| change_hashes(changes).ok())
        .flatten()
        .collect();
    if !made.is_subset(&governed) && made.iter().all(|hash| live.workspace_holds_change(hash)) {
        return Ok(());
    }
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    publication.governance.clear();
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    Ok(())
}

pub(crate) fn refresh_governance_checkpoint(
    live: &StatePeer,
    view: &mut stored::StoredWorkspace,
) -> Result<(), String> {
    let Some(existing) = state_of(view)?.governance.first().cloned() else {
        return Ok(());
    };
    // A member's own changes stay as it made them until they are admitted.
    if existing.incremental {
        return Ok(());
    }
    let bytes = encode(&live.workspace_message(), MAX_BYTES)?;
    let binding = existing.binding.clone();
    if bytes == existing.bytes && binding == existing.binding {
        return Ok(());
    }
    let total = bounds(state_of(view)?)?;
    if total
        .checked_sub(existing.bytes.len())
        .and_then(|size| size.checked_add(bytes.len()))
        .is_none_or(|size| size > MAX_BYTES)
    {
        return Err("refreshed native governance checkpoint exceeds outgoing capacity".into());
    }
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    publication.governance = vec![StoredOutgoing {
        id: *blake3::hash(&bytes).as_bytes(),
        bytes,
        binding,
        ..existing
    }];
    Ok(())
}
/// Take `bytes`, one of this device's writes, into ownership: at its head while the head has room
/// and nothing waits behind it, else at its tail, which the store keeps in runs once it is long
/// ([`settle`]). Ownership refuses no write for how many it holds.
fn append(view: &mut stored::StoredWorkspace, bytes: Vec<u8>) -> Result<(), String> {
    let id = *blake3::hash(&bytes).as_bytes();
    let workspace = view.workspace;
    let ownership = state_of(view)?;
    if ownership
        .entries
        .iter()
        .chain(&ownership.tail)
        .any(|entry| entry.id == id && entry.bytes == bytes)
    {
        return Ok(());
    }
    let head = bounds(ownership)?;
    let message = NeighbourMsg::decode(&bytes).map_err(|e| e.to_string())?;
    let (timestamp_ns, channel, incremental) = metadata(&message)?;
    let binding = request(&message)?.binding()?;
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    let at_head = publication.tail.is_empty()
        && publication.spilled.is_empty()
        && publication.entries.len() < MAX_ENTRIES
        && head
            .checked_add(bytes.len())
            .is_some_and(|size| size <= MAX_BYTES);
    let ordinal = publication.next_ordinal;
    publication.next_ordinal = ordinal
        .checked_add(1)
        .ok_or("native publication ordinal exhausted")?;
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    let entry = StoredOutgoing {
        ordinal,
        id,
        bytes,
        binding,
        timestamp_ns,
        channel,
        incremental,
    };
    if at_head {
        publication.entries.push(entry);
    } else {
        // Nothing is lost: the write waits behind the others, kept beside the record, and leaves
        // as the workspace's host answers those. Said once each time ownership fills.
        if publication.tail.is_empty() && publication.spilled.is_empty() {
            tracing::warn!(
                %workspace,
                waiting = publication.entries.len(),
                "this device's writes wait to be sent: the workspace's host has not answered the \
                 ones before them"
            );
        }
        publication.tail.push(entry);
    }
    Ok(())
}

/// Keep the oldest of `tail` as runs `store` keeps beside `workspace`'s record while it holds a
/// run's worth (`len` gives each one's bytes), and answer each run's hash and length.
fn spill<T: Serialize>(
    tail: &mut Vec<T>,
    len: impl Fn(&T) -> usize,
    store: &dyn super::Store,
    workspace: WorkspaceId,
) -> Result<Vec<([u8; 32], usize)>, String> {
    let mut runs = Vec::new();
    while tail.len() >= MAX_ENTRIES || tail.iter().map(&len).sum::<usize>() >= RUN_BYTES {
        let mut bytes = 0usize;
        let count = tail
            .iter()
            .take(MAX_ENTRIES)
            .take_while(|item| {
                let fits = bytes == 0 || bytes.saturating_add(len(item)) <= RUN_BYTES;
                bytes = bytes.saturating_add(len(item));
                fits
            })
            .count();
        let run: Vec<T> = tail.drain(..count).collect();
        let encoded = postcard::to_allocvec(&run).map_err(|e| e.to_string())?;
        runs.push((store.put_ownership(workspace, &encoded)?, count));
    }
    Ok(runs)
}

/// Keep `workspace`'s ownership in shape before it is published: its head takes the oldest writes
/// waiting behind it while it has room, and a tail that grew long becomes a stretch the store keeps
/// beside the workspace. Every write stays in the order it was committed.
///
/// # Errors
/// A stretch could not be kept or read back.
pub(crate) fn settle(
    view: &mut stored::StoredWorkspace,
    (store, workspace, read): Ownership<'_>,
) -> Result<(), String> {
    let Some(publication) = view.publication.as_mut() else {
        return Ok(());
    };
    let mut held = bounds(publication)?;
    let mut changed = false;
    let mut run: Option<std::sync::Arc<Vec<StoredOutgoing>>> = None;
    while publication.entries.len() < MAX_ENTRIES {
        let next = match publication.spilled.first() {
            Some(front) => {
                let writes = match &mut run {
                    Some(writes) => writes,
                    None => run.insert(read.read(store, workspace, front)?),
                };
                writes
                    .get(usize::try_from(front.taken).map_err(|e| e.to_string())?)
                    .cloned()
                    .ok_or("a kept run of ownership is shorter than its record says")?
            }
            None => match publication.tail.first() {
                Some(first) => first.clone(),
                None => break,
            },
        };
        let Some(after) = held
            .checked_add(next.bytes.len())
            .filter(|size| *size <= MAX_BYTES)
        else {
            break;
        };
        match publication.spilled.first_mut() {
            Some(front) => {
                front.taken = front.taken.saturating_add(1);
                if front.taken == front.writes {
                    publication.spilled.remove(0);
                    run = None;
                }
            }
            None => {
                publication.tail.remove(0);
            }
        }
        held = after;
        publication.entries.push(next);
        changed = true;
    }
    for (hash, count) in spill(
        &mut publication.tail,
        |entry| entry.bytes.len(),
        store,
        workspace,
    )? {
        publication.spilled.push(SpilledRun {
            hash,
            writes: u32::try_from(count).map_err(|e| e.to_string())?,
            taken: 0,
        });
        changed = true;
    }
    if changed {
        publication.revision = publication
            .revision
            .checked_add(1)
            .ok_or("native publication revision exhausted")?;
    }
    Ok(())
}

/// The name a store keeps the run of `workspace`'s ownership named `hash` under
/// ([`super::Store::put_ownership`]). It is the name stores have kept such writes under since
/// they kept any, which is not what the code calls them: a store finds what it wrote by it.
#[must_use]
pub fn ownership_run_name(workspace: WorkspaceId, hash: [u8; 32]) -> String {
    kept_name(workspace, "custody", hash)
}

fn kept_name(workspace: WorkspaceId, kept_as: &str, hash: [u8; 32]) -> String {
    format!(
        "{workspace}.{kept_as}.{}",
        blake3::Hash::from_bytes(hash).to_hex()
    )
}

impl stored::NativeAuthorityStorage {
    /// The name of every run of outgoing ownership this record names: what its store keeps
    /// beside it, and may let go of once a record it publishes no longer names it.
    pub fn ownership_runs(&self) -> impl Iterator<Item = String> + '_ {
        self.workspaces.iter().flat_map(|(workspace, stored)| {
            kept_hashes(stored).map(move |hash| ownership_run_name(*workspace, hash))
        })
    }
}

/// The writes of every stretch `publication` keeps beside its record that are not at its head yet.
pub(crate) fn spilled_writes(
    publication: &StoredPublication,
    (store, workspace, read): Ownership<'_>,
) -> Result<Vec<StoredOutgoing>, String> {
    let mut writes = Vec::new();
    for spilled in &publication.spilled {
        let taken = usize::try_from(spilled.taken).map_err(|e| e.to_string())?;
        writes.extend(
            read.read(store, workspace, spilled)?
                .iter()
                .skip(taken)
                .cloned(),
        );
    }
    Ok(writes)
}

/// The ids of every write `publication` holds, in the order it sends them.
pub(crate) fn ownership_ids(
    publication: &StoredPublication,
    (store, workspace, read): Ownership<'_>,
) -> Result<Vec<[u8; 32]>, String> {
    let mut ids: Vec<[u8; 32]> = publication.entries.iter().map(|entry| entry.id).collect();
    for spilled in &publication.spilled {
        let taken = usize::try_from(spilled.taken).map_err(|e| e.to_string())?;
        ids.extend(
            read.read(store, workspace, spilled)?
                .iter()
                .skip(taken)
                .map(|write| write.id),
        );
    }
    ids.extend(publication.tail.iter().map(|entry| entry.id));
    Ok(ids)
}

/// The stretches of ownership's backlog `view` names, which its store keeps while it does.
pub(crate) fn kept_hashes(view: &stored::StoredWorkspace) -> impl Iterator<Item = [u8; 32]> + '_ {
    view.publication.iter().flat_map(|publication| {
        publication
            .spilled
            .iter()
            .map(|spilled| spilled.hash)
            .chain(
                publication
                    .hosted
                    .iter()
                    .flat_map(|held| held.runs.iter().copied()),
            )
    })
}
pub(crate) fn append_local(
    view: &mut stored::StoredWorkspace,
    bytes: Vec<u8>,
) -> Result<(), String> {
    append(view, bytes)
}
pub(crate) fn has_exact(
    view: &stored::StoredWorkspace,
    bytes: &[u8],
    runs: Ownership<'_>,
) -> Result<bool, String> {
    let publication = state_of(view)?;
    let found = |kept: &[StoredOutgoing]| kept.iter().any(|entry| entry.bytes == bytes);
    if found(&publication.entries) || found(&publication.tail) {
        return Ok(true);
    }
    Ok(found(&spilled_writes(publication, runs)?))
}
// qps validates historical ownership separately from current send/sign permission.
fn validate_exact(
    peer: &StatePeer,
    message: &NeighbourMsg,
    device: [u8; 32],
) -> Result<(), String> {
    peer.outbox()
        .lookup_retained_local_document(message, device)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "native outgoing entry lacks exact committed admission".to_owned())
        .map(|_| ())
}
fn retired(peer: &StatePeer, message: &NeighbourMsg, device: [u8; 32]) -> Result<bool, String> {
    peer.outbox()
        .retained_local_document_retired(message, device)
}
pub(crate) fn decode_entries(entries: &[StoredOutgoing]) -> Result<Vec<NeighbourMsg>, String> {
    entries
        .iter()
        .map(|entry| NeighbourMsg::decode(&entry.bytes).map_err(|e| e.to_string()))
        .collect()
}

pub(crate) fn validate(view: &NativeCommittedView) -> Result<(), String> {
    let publication = state(view)?;
    bounds(publication)?;
    let mut ids = BTreeSet::new();
    let mut ordinals = BTreeSet::new();
    for entry in publication
        .governance
        .iter()
        .chain(publication.entries.iter())
        .chain(publication.tail.iter())
    {
        if entry.ordinal == 0
            || entry.ordinal >= publication.next_ordinal
            || !ids.insert(entry.id)
            || !ordinals.insert(entry.ordinal)
            || entry.id != *blake3::hash(&entry.bytes).as_bytes()
        {
            return Err("native outgoing identity/order mismatch".into());
        }
        let message: NeighbourMsg =
            NeighbourMsg::decode(&entry.bytes).map_err(|e| e.to_string())?;
        if publication
            .governance
            .iter()
            .any(|item| item.id == entry.id)
        {
            validate_governance(view, entry)?;
        } else if encode(&message, MAX_BYTES)? != entry.bytes
            || request(&message)?.binding()? != entry.binding
            || metadata(&message)? != (entry.timestamp_ns, entry.channel, entry.incremental)
        {
            return Err("native outgoing bytes/binding mismatch".into());
        }
        if !publication
            .governance
            .iter()
            .any(|item| item.id == entry.id)
        {
            validate_exact(&view.peer, &message, view.device().device_public_key)?;
        }
    }
    let written: Vec<u64> = publication
        .entries
        .iter()
        .chain(&publication.tail)
        .map(|entry| entry.ordinal)
        .collect();
    if written
        .windows(2)
        .any(|pair| matches!(pair, [first, second] if first >= second))
    {
        return Err("native publication ordinal order mismatch".into());
    }
    Ok(())
}

fn validate_governance(view: &NativeCommittedView, entry: &StoredOutgoing) -> Result<(), String> {
    if entry.incremental {
        return validate_member_changes(view, entry);
    }
    let message = NeighbourMsg::decode(&entry.bytes).map_err(|e| e.to_string())?;
    if !matches!(&message, NeighbourMsg::SyncResp { workspace, .. } if workspace == &view.record.workspace.to_string())
        || encode(&view.peer.workspace_message(), MAX_BYTES)? != entry.bytes
        || entry.id != *blake3::hash(&entry.bytes).as_bytes()
        || entry.incremental
    {
        return Err("native governance checkpoint differs from canonical workspace policy".into());
    }
    let (channel, retention) = neighbour_wire::retention(&message, true)
        .ok_or("unsupported native governance checkpoint")?;
    if retention != Retention::Checkpoint || channel != entry.channel {
        return Err("native governance checkpoint transport mismatch".into());
    }
    if let crate::owner_commit::CommitTarget::NativeCheckpointRoot { root } = entry.binding.target {
        if view.peer.workspace().workspace_checkpoint_root()? != Some(root)
            || entry.binding != checkpoint_root_binding(view.record.workspace, root)
        {
            return Err("native checkpoint publication differs from signed root".into());
        }
        return Ok(());
    }
    if !receipted(view, &entry.binding) {
        return Err("native governance checkpoint lacks its owner receipt".into());
    }
    Ok(())
}

/// The binding a publication of a workspace's checkpoint root carried. No
/// device anchors new checkpoint content, but a workspace whose root was
/// anchored before still publishes it as it was signed.
fn checkpoint_root_binding(workspace: WorkspaceId, address: [u8; 32]) -> CommitBinding {
    CommitBinding {
        workspace: workspace.to_string(),
        target: crate::owner_commit::CommitTarget::NativeCheckpointRoot { root: address },
        request_digest: *blake3::Hasher::new_derive_key(
            "Tilo native checkpoint root publication v1",
        )
        .update(&address)
        .finalize()
        .as_bytes(),
        basis: address,
    }
}

/// Whether `binding` is one this device receipted: a membership command, a
/// broker effect, a shortcut change or a join admission it committed.
fn receipted(view: &NativeCommittedView, binding: &CommitBinding) -> bool {
    view.record
        .workspace_memberships
        .values()
        .any(|receipt| &receipt.receipt.binding == binding)
        || view
            .record
            .effect_receipts
            .values()
            .any(|receipt| &receipt.receipt.binding == binding)
        || view
            .record
            .shortcut_change
            .as_ref()
            .is_some_and(|receipt| &receipt.binding == binding)
        || view
            .record
            .join_admissions
            .values()
            .any(|receipt| &receipt.receipt.binding == binding)
}
/// A member's published workspace changes: a delta of changes this device made,
/// every one of them in the workspace it holds, bound to its membership receipt.
fn validate_member_changes(
    view: &NativeCommittedView,
    entry: &StoredOutgoing,
) -> Result<(), String> {
    let message = NeighbourMsg::decode(&entry.bytes).map_err(|e| e.to_string())?;
    let NeighbourMsg::SyncResp { changes, workspace } = &message else {
        return Err("native membership publication is not a workspace delta".into());
    };
    if workspace != &view.record.workspace.to_string()
        || encode(&message, MAX_BYTES)? != entry.bytes
        || entry.id != *blake3::hash(&entry.bytes).as_bytes()
    {
        return Err("native membership publication differs from its workspace".into());
    }
    // The actor this replica writes under: who wrote a change is never
    // read from its actor, only which copy of the workspace made it.
    let made_here = view
        .peer
        .workspace_actor_for(&view.device().device_public_key);
    for bytes in changes {
        let change = automerge::Change::from_bytes(bytes.clone()).map_err(|e| e.to_string())?;
        if change.actor_id() != &made_here || !view.peer.workspace_holds_change(&change.hash()) {
            return Err("native membership publication carries a change not made here".into());
        }
    }
    if neighbour_wire::retention(&message, false) != Some((entry.channel, Retention::Delta)) {
        return Err("native membership publication transport mismatch".into());
    }
    if !receipted(view, &entry.binding) {
        return Err("native membership publication lacks its receipt".into());
    }
    Ok(())
}
/// An exact replay changes no peer state; reuse its barrier only when there
/// is no retirement or checkpoint refresh to publish.
pub(crate) fn exact_replay_unchanged(
    live: &StatePeer,
    view: &stored::StoredWorkspace,
) -> Result<bool, String> {
    let publication = state_of(view)?;
    let device = view.device.device_public_key;
    let originals = decode_entries(&publication.entries)?;
    for message in &originals {
        if retired(live, message, device).unwrap_or(false) {
            return Ok(false);
        }
    }
    // A member's own workspace changes wait for the governance that retires
    // them, which any replay may bring.
    if let Some(checkpoint) = publication.governance.first()
        && (checkpoint.incremental
            || encode(&live.workspace_message(), MAX_BYTES)? != checkpoint.bytes)
    {
        return Ok(false);
    }
    Ok(true)
}

impl StoredPublication {
    /// What the host holds of `record` of `app` that the record holds here only inside a
    /// compaction of this device's own; `None` while the record's history is the host's.
    /// `(store, workspace)` keep its runs.
    fn hosted(
        &self,
        (store, workspace, _): Ownership<'_>,
        (app, record): (&str, &str),
    ) -> Result<Option<Hosted>, String> {
        let Some(held) = self
            .hosted
            .iter()
            .find(|held| held.app == app && held.record == record)
        else {
            return Ok(None);
        };
        let mut writes = Vec::new();
        for hash in &held.runs {
            let run: Vec<HostedWrite> =
                postcard::from_bytes(&store.read_ownership(workspace, *hash)?)
                    .map_err(|e| e.to_string())?;
            writes.extend(run);
        }
        Ok(Some(Hosted {
            writes: writes
                .iter()
                .chain(&held.writes)
                .map(|write| postcard::from_bytes(&write.0).map_err(|e| e.to_string()))
                .collect::<Result<_, String>>()?,
            covered: held.covered.iter().copied().collect(),
        }))
    }
    fn set_hosted(
        &mut self,
        runs: Ownership<'_>,
        (app, record): (&str, &str),
        hosted: Option<&Hosted>,
    ) -> Result<(), String> {
        self.hosted
            .retain(|held| held.app != app || held.record != record);
        if let Some(hosted) = hosted {
            let writes = hosted
                .writes
                .iter()
                .map(|write| {
                    postcard::to_stdvec(write)
                        .map(HostedWrite)
                        .map_err(|e| e.to_string())
                })
                .collect::<Result<_, _>>()?;
            let mut held = HostedHistory {
                app: app.to_owned(),
                record: record.to_owned(),
                runs: Vec::new(),
                writes,
                covered: hosted.covered.iter().map(|(a, s)| (*a, *s)).collect(),
            };
            held.spill(runs)?;
            self.hosted.push(held);
        }
        Ok(())
    }
    /// Take `write`, one of this device's that the host now holds, into what this device knows
    /// of the host's history of `record` of `app`, unless the record holds it as itself or a
    /// compaction of the host's covers it.
    fn hosted_write(
        &mut self,
        live: &StatePeer,
        runs: Ownership<'_>,
        (app, record): (&str, &str),
        write: &SignedOp,
    ) -> Result<(), String> {
        let Some(held) = self
            .hosted
            .iter_mut()
            .find(|held| held.app == app && held.record == record)
        else {
            return Ok(());
        };
        if held
            .covered
            .iter()
            .any(|(author, seq)| author == write.author.as_bytes() && write.seq <= *seq)
            || live.publication().holds_sealed(app, record, write)
        {
            return Ok(());
        }
        held.writes.push(HostedWrite(
            postcard::to_stdvec(write).map_err(|e| e.to_string())?,
        ));
        held.spill(runs)
    }
    /// Count `answer` toward the writes to `app`'s `record`.
    fn answer(&mut self, app: &str, record: &str, answer: HostAnswer) {
        let key = (app.to_owned(), record.to_owned());
        let mut held = self.answered.get(&key).copied().unwrap_or_default();
        match answer {
            HostAnswer::Durable(order) => {
                held.durable = held.durable.saturating_add(1);
                held.last_order = Some(order);
            }
            HostAnswer::Refused => held.refused = held.refused.saturating_add(1),
        }
        self.answered.insert(key, held);
    }

    /// What a journal entry carries apart from the rest of the workspace's state
    /// ([`super::journal`]): the server's answers, the writes it has not taken, and the lists of
    /// writes ownership holds (its governance, its head and its tail), all taken out of this.
    pub(crate) fn take_journaled(&mut self) -> Journaled {
        Journaled {
            answered: std::mem::take(&mut self.answered),
            unanswered: std::mem::take(&mut self.unanswered),
            writes: [
                std::mem::take(&mut self.governance),
                std::mem::take(&mut self.entries),
                std::mem::take(&mut self.tail),
            ],
        }
    }

    /// Hold `journaled` again, as [`Self::take_journaled`] took it.
    pub(crate) fn set_journaled(&mut self, journaled: Journaled) {
        let Journaled {
            answered,
            unanswered,
            writes: [governance, entries, tail],
        } = journaled;
        self.answered = answered;
        self.unanswered = unanswered;
        self.governance = governance;
        self.entries = entries;
        self.tail = tail;
    }
}

/// The parts of ownership a journal entry carries as what changed of them
/// ([`StoredPublication::take_journaled`]).
#[derive(Default)]
pub(crate) struct Journaled {
    pub(crate) answered: Answered,
    pub(crate) unanswered: Unanswered,
    /// Ownership's governance, its head and its tail, each in order.
    pub(crate) writes: [Vec<StoredOutgoing>; 3],
}
pub(crate) fn retire(live: &StatePeer, view: &mut stored::StoredWorkspace) -> Result<bool, String> {
    let publication = state_of(view)?;
    let mut remove = BTreeSet::new();
    for entry in &publication.entries {
        let message: NeighbourMsg =
            NeighbourMsg::decode(&entry.bytes).map_err(|e| e.to_string())?;
        // A refused query cannot discard newly admitted revocations or prerequisites.
        if retired(live, &message, view.device.device_public_key).unwrap_or(false) {
            remove.insert(entry.id);
        }
    }
    if remove.is_empty() {
        return Ok(false);
    }
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    publication
        .entries
        .retain(|entry| !remove.contains(&entry.id));
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    Ok(true)
}
pub(crate) fn retry_covered(
    live: &StatePeer,
    view: &stored::StoredWorkspace,
    bytes: &[u8],
) -> Result<bool, String> {
    let message = NeighbourMsg::decode(bytes).map_err(|e| e.to_string())?;
    retired(live, &message, view.device.device_public_key)
}
impl NativeWorkspaceOwner {
    /// Read `workspace`'s outgoing ownership: what the server session may send now.
    pub fn with_outgoing<T>(
        &self,
        workspace: WorkspaceId,
        now: SystemTime,
        read: impl FnOnce(&NativeCommittedView, NativeOutgoing<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        // What earlier passes checked here, out of the map while it is read and pruned to what
        // ownership still holds: the set stays as small as the head.
        let mut verified = self
            .0
            .verified
            .lock()
            .remove(&workspace)
            .unwrap_or_default();
        let result = self.with_current(workspace, |view| {
            view.peer
                .permissions(view.device().device_public_key, now)
                .map_err(|error| error.to_string())?;
            let publication = state(view)?;
            verified.retain(|id| {
                publication
                    .governance
                    .iter()
                    .chain(publication.entries.iter())
                    .any(|entry| entry.id == *id)
            });
            read(
                view,
                NativeOutgoing {
                    state: publication,
                    allowed: allowed(view, now, &mut verified)?,
                },
            )
        });
        self.0.verified.lock().insert(workspace, verified);
        result
    }
    /// Whether this device owes `workspace` nothing: its outgoing ownership holds no write and no
    /// governance entry, sendable or waiting. Read without judging the device's permissions, which
    /// only a write that goes needs: a device that entered a workspace and wrote nothing to it
    /// may not be on its roster yet, and has nothing to be judged for.
    ///
    /// # Errors
    /// The workspace is not held here, or its ownership is missing.
    pub fn owes_nothing(&self, workspace: WorkspaceId) -> Result<bool, String> {
        self.with_current(workspace, |view| {
            let publication = state(view)?;
            Ok(publication.governance.is_empty()
                && publication.entries.is_empty()
                && publication.tail.is_empty()
                && publication
                    .spilled
                    .iter()
                    .all(|run| run.taken >= run.writes))
        })
    }
    /// How many of this device's committed writes to `workspace` are still in
    /// its outgoing ownership: not yet admitted by the workspace's governor.
    pub fn unadmitted_writes(&self, workspace: WorkspaceId) -> Result<usize, String> {
        self.with_current(workspace, |view| {
            let publication = state(view)?;
            Ok(publication
                .spilled
                .iter()
                .map(|run| run.writes.saturating_sub(run.taken) as usize)
                .fold(
                    publication
                        .entries
                        .len()
                        .saturating_add(publication.tail.len()),
                    usize::saturating_add,
                ))
        })
    }
    /// Every write of this device to `workspace` still in its outgoing ownership, sendable now or
    /// not, by id, in the order it sends them: what a caller waits on to know all of them reached
    /// the workspace's host.
    ///
    /// # Errors
    /// The workspace is not held here, or ownership's backlog could not be read.
    pub fn owed_writes(&self, workspace: WorkspaceId) -> Result<Vec<[u8; 32]>, String> {
        self.with_current(workspace, |view| {
            let publication = state(view)?;
            ownership_ids(publication, self.ownership(workspace))
        })
    }
    /// The write in `workspace`'s ownership that carries this device's governance of it, by id:
    /// the changes it made to who is in the workspace, as one of its members or as the device of
    /// its owner. It stays in ownership after the host answers it, so a caller that waits for the
    /// host to hold it waits for that answer.
    ///
    /// # Errors
    /// The workspace is not held here.
    pub fn governance_write(&self, workspace: WorkspaceId) -> Result<Option<[u8; 32]>, String> {
        self.with_current(workspace, |view| {
            Ok(state(view)?.governance.first().map(|entry| entry.id))
        })
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn outgoing_status(
        &self,
        workspace: WorkspaceId,
        now: SystemTime,
    ) -> Result<NativeOutgoingStatus, String> {
        self.with_current(workspace, |view| status(view, now))
    }
    /// Retire every retained frame the workspace has refused or a durable
    /// receipt names, and commit the retirement.
    pub fn retire_outgoing(&self, workspace: WorkspaceId) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let now = tilo_clock::wall();
        candidate
            .peer
            .permissions(candidate.device().device_public_key, now)
            .map_err(|error| error.to_string())?;
        let (changed, mut record) = candidate.change(|live, record| retire(live, record))?;
        if !changed {
            return Ok(());
        }
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_document(workspace, &candidate, record, None)
    }
    /// Retire the ownership entries `answers` names, which the workspace's server host answered:
    /// a server host acknowledges each commit, durable or refused, rather than receipting it in
    /// the workspace's governance, and an answered entry is never sent again. Each answer
    /// counts toward its record's [`DocumentWrites`], and a refused write leaves this device's
    /// replica as well: the workspace never holds it, so neither does this device.
    ///
    /// # Errors
    /// The workspace is not held here, or the retirement could not be committed.
    pub fn retire_answered(
        &self,
        workspace: WorkspaceId,
        answers: &BTreeMap<[u8; 32], HostAnswer>,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let now = tilo_clock::wall();
        let runs = self.ownership(workspace);
        let (changed, mut record) = candidate.change(|live, record| {
            let publication = record.publication.as_mut().ok_or("publication absent")?;
            // A refused compaction keeps its place, and is drafted again there against what
            // the host holds now: no later write of its record has left before it.
            let again: Vec<[u8; 32]> = publication
                .entries
                .iter()
                .filter(|entry| {
                    answers.get(&entry.id) == Some(&HostAnswer::Refused) && is_compaction(entry)
                })
                .map(|entry| entry.id)
                .collect();
            let (answered, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut publication.entries)
                .into_iter()
                .partition(|entry| answers.contains_key(&entry.id) && !again.contains(&entry.id));
            publication.entries = kept;
            publication.slots.extend(&again);
            if answered.is_empty() && again.is_empty() {
                return Ok(false);
            }
            for entry in answered {
                // Ownership holds governance entries too, which belong to no record.
                let Ok(NeighbourMsg::AppDocument {
                    app_id,
                    document_id,
                    envelope,
                    ..
                }) = NeighbourMsg::decode(&entry.bytes)
                else {
                    continue;
                };
                let answer = answers
                    .get(&entry.id)
                    .copied()
                    .unwrap_or(HostAnswer::Refused);
                let route = (app_id.as_str(), document_id.as_str());
                match (envelope.kind, answer) {
                    // A segment is taken with its compaction.
                    (OpKind::Compaction, _) if collab_types::is_compaction_segment(&envelope) => {}
                    (OpKind::Compaction, answer) => {
                        let owed = owed_to(publication, runs, route)?;
                        for segment in owed.iter().filter(|write| {
                            collab_types::is_compaction_segment(write)
                                && write.author == envelope.author
                                && write.seq == envelope.seq
                        }) {
                            live.publication_mut()
                                .gather_sealed_piece(route, segment.clone())?;
                        }
                        take_hosted(live, publication, runs, route, &envelope, &owed, now)?;
                        // A refused compaction is drafted again in its place, and stays one
                        // its server has not taken until it takes another.
                        if matches!(answer, HostAnswer::Durable(_)) {
                            publication.answered_write(&envelope);
                        }
                    }
                    (_, HostAnswer::Refused) => {
                        live.withdraw_local_document(&app_id, &document_id, &envelope)?;
                        publication.answered_write(&envelope);
                    }
                    (_, HostAnswer::Durable(_)) => {
                        publication.hosted_write(live, runs, route, &envelope)?;
                        publication.answered_write(&envelope);
                    }
                }
                publication.answer(&app_id, &document_id, answer);
            }
            publication.revision = publication
                .revision
                .checked_add(1)
                .ok_or("native publication revision exhausted")?;
            Ok(true)
        })?;
        if !changed {
            return Ok(());
        }
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_document(workspace, &candidate, record, None)
    }
    /// This device's writes to each record of `workspace` it wrote, by app and record: those
    /// in its ownership, and the server host's answers to the others.
    pub fn document_writes(
        &self,
        workspace: WorkspaceId,
    ) -> BTreeMap<(String, String), DocumentWrites> {
        self.with_current(workspace, |view| {
            let publication = state(view)?;
            let mut writes: BTreeMap<(String, String), DocumentWrites> = publication
                .answered
                .iter()
                .map(|(key, answered)| {
                    (
                        key.clone(),
                        DocumentWrites {
                            pending: 0,
                            durable: answered.durable,
                            refused: answered.refused,
                            last_order: answered.last_order,
                        },
                    )
                })
                .collect();
            let spilled = spilled_writes(publication, self.ownership(workspace))?;
            for entry in publication
                .entries
                .iter()
                .chain(&spilled)
                .chain(&publication.tail)
            {
                if let Ok(NeighbourMsg::AppDocument {
                    app_id,
                    document_id,
                    ..
                }) = tilo_failure::ignored!(
                    "ownership holds governance entries too, which belong to no record",
                    NeighbourMsg::decode(&entry.bytes)
                ) {
                    let writes = writes.entry((app_id, document_id)).or_default();
                    writes.pending = writes.pending.saturating_add(1);
                }
            }
            Ok(writes)
        })
        .unwrap_or_default()
    }
    /// Renew this device's sealed writes that expired held before the
    /// governor retired them, with the keys [`Self::keep_sealed_histories`] gave
    /// (see [`PeerOutbox::draft_sealed_renewals`](crate::PeerOutbox::draft_sealed_renewals)).
    /// Each renewal replaces its original in one durable commit and moves behind every other
    /// entry, so an author's writes still send in sequence order. A write the keys do not renew
    /// stays exactly as it was. Answers how many writes were renewed.
    ///
    /// # Errors
    /// As [`Self::commit_outgoing`].
    pub async fn renew_outgoing(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        let Some(keys) = self.0.history_keys.lock().clone() else {
            return Ok(0);
        };
        let draft = |view: &NativeCommittedView| {
            let mut reseal =
                |(app, document): (&str, &str), named: Option<&str>, write: &SignedOp| {
                    if view.peer.requires_document_keys(app, document, now)? {
                        let route = (app, document);
                        let readers = view.peer.document_key_holders_read_by(route, named, now)?;
                        keys.renew_scoped(workspace, route, named, &readers, write)
                    } else {
                        let readers = named.unwrap_or(collab_types::EVERY_MEMBER);
                        let group = collab_types::reader_group(workspace, readers)
                            .ok_or("invalid reader set for write renewal")?;
                        keys.renew(group, write)
                    }
                };
            let entries = &state_of(&view.record)?.entries;
            let originals = entries
                .iter()
                .map(|entry| NeighbourMsg::decode(&entry.bytes).map_err(|e| e.to_string()))
                .collect::<Result<Vec<NeighbourMsg>, String>>()?;
            let device = view.device().device_public_key;
            let drafts =
                view.peer
                    .outbox()
                    .draft_sealed_renewals(&originals, &mut reseal, device, now);
            Ok(entries
                .iter()
                .zip(originals)
                .zip(drafts)
                .filter(|((_, original), _)| {
                    // A compaction is drafted when it is sent, under the epoch of that moment.
                    !matches!(original, NeighbourMsg::AppDocument { envelope, .. }
                        if envelope.kind == OpKind::Compaction)
                })
                .filter_map(|((entry, original), draft)| {
                    // The write stays valid only until its own expiry: a person whose change
                    // does not reach the workspace needs this in the journal.
                    let renewal = draft
                        .inspect_err(|error| tracing::warn!(%error, "sealed write stays unrenewed"))
                        .ok()??;
                    Some((Some(entry.id), original, vec![renewal]))
                })
                .collect())
        };
        let admit = |live: &mut StatePeer,
                     publication: &mut StoredPublication,
                     original: NeighbourMsg,
                     renewal: Vec<SignedOp>| {
            let renewal = renewal.into_iter().next().ok_or("no renewal drafted")?;
            let message =
                live.outbox_mut()
                    .admit_sealed_renewal(&original, renewal.clone(), now)?;
            if let NeighbourMsg::AppDocument { envelope, .. } = &original {
                // The document holds the renewal in the original's place, and its server has
                // neither.
                publication.answered_write(envelope);
                publication.unanswered_write(&renewal);
            }
            Ok(message)
        };
        self.commit_outgoing(workspace, signer, now, draft, admit)
            .await
    }

    /// Keep every sealed record history this owner holds within its bound
    /// with `keys`, the channel keys this device holds: before each of this
    /// device's writes, and whenever its holder asks
    /// ([`Self::compact_outgoing`]). Compaction does not wait for a link, so a
    /// device alone or offline goes on writing however long a record's
    /// history grows.
    pub fn keep_sealed_histories(&self, keys: Arc<dyn SealedHistoryKeys>) {
        *self.0.history_keys.lock() = Some(keys);
    }

    /// Compact every sealed record history of `workspace` that has used most of
    /// its bound into one sealed checkpoint, and commit the compactions
    /// into this device's outgoing ownership, where they publish as the
    /// checkpoints they are, after the writes they cover. The keys
    /// [`Self::keep_sealed_histories`] gave open any write of a history this
    /// device can read and seal a record's merged snapshot under its channel's
    /// current epoch
    /// ([`PeerPublication::draft_sealed_compaction`](crate::PeerPublication::draft_sealed_compaction)).
    /// A record whose keys are not here, or whose history does not merge yet,
    /// waits: a workspace none of whose due histories compacted is tried again
    /// only after [`COMPACTION_RETRY`] more passes, as merging a whole history
    /// on every keystroke would cost more than the write.
    ///
    /// Answers how many histories were compacted. Every device taking part
    /// maintains its own copy this way; no host compacts for it.
    ///
    /// # Errors
    /// As [`Self::commit_outgoing`].
    pub async fn compact_outgoing(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        self.compact(workspace, None, signer, now).await
    }

    /// [`Self::compact_outgoing`], with `record` of `app` compacted now whatever share of its
    /// bound its history used: the checkpoint a workspace's host asks of a member is this
    /// compaction, which every replica takes in place of exactly the writes it covers, so a
    /// record's history never holds a checkpoint beside the writes it re-states.
    ///
    /// # Errors
    /// As [`Self::commit_outgoing`].
    pub async fn compact_document(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        let route = (app.to_owned(), record.to_owned());
        self.compact(workspace, Some(route), signer, now).await
    }

    /// Ownership's entry of this device's latest compaction of `record` of `app` in `workspace`,
    /// once it is drafted to send: `None` while it waits for its place, or when ownership holds
    /// none.
    ///
    /// # Errors
    /// The workspace is not held here.
    pub fn drafted_compaction(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
    ) -> Result<Option<Vec<u8>>, String> {
        self.with_current(workspace, |view| {
            let publication = state(view)?;
            let latest = publication.entries.iter().rev().find(|entry| {
                NeighbourMsg::decode(&entry.bytes).is_ok_and(|message| {
                    matches!(&message, NeighbourMsg::AppDocument { app_id, document_id, envelope, .. }
                        if app_id == app && document_id == record && envelope.kind == OpKind::Compaction
                            && !collab_types::is_compaction_segment(envelope))
                })
            });
            Ok(latest
                .filter(|entry| !publication.slots.contains(&entry.id))
                .map(|entry| entry.bytes.clone()))
        })
    }

    async fn compact(
        &self,
        workspace: WorkspaceId,
        asked: Option<(String, String)>,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        let Some(keys) = self.0.history_keys.lock().clone() else {
            return Ok(0);
        };
        self.draft_next(workspace, signer, keys.as_ref(), now)
            .await?;
        // A record asked for is compacted at once, whatever share of its bound it used.
        if asked.is_none()
            && self.with_current(workspace, |view| {
                Ok(view.peer.publication().sealed_to_compact().is_empty())
            })?
        {
            return Ok(0);
        }
        // A history that drafted nothing rests a while: folding it again on every pass would
        // read the whole history as often.
        let resting = if asked.is_none() {
            self.0.resting.pass(workspace)
        } else {
            std::collections::BTreeSet::new()
        };
        let failed = std::sync::Mutex::new(Vec::new());
        let folded = std::sync::Mutex::new(Vec::new());
        let runs = self.ownership(workspace);
        let draft = |view: &NativeCommittedView| {
            let device = view.device().device_public_key;
            let mut due = view.peer.publication().sealed_to_compact();
            if let Some(route) = asked.as_ref()
                && !due.contains(route)
            {
                due.push(route.clone());
            }
            due.into_iter()
                .filter_map(|(app, record)| {
                    if asked.is_none() && resting.contains(&(app.clone(), record.clone())) {
                        return None;
                    }
                    let route = (app.as_str(), record.as_str());
                    let named = keys.named_readers(workspace, &record);
                    if neighbour_wire::KeyScope::has_named_readers(&record) && named.is_none() {
                        return None;
                    }
                    let group = collab_types::reader_group(workspace, named.as_deref().unwrap_or(collab_types::EVERY_MEMBER))?;
                    let mut open = |write: &SignedOp| keys.open(group, write);
                    let mut seal = |snapshot: &[u8]| {
                        seal_for_document(&*keys, &view.peer, workspace, route, named.as_deref(), snapshot, now)
                    };
                    let ops = view
                        .peer
                        .publication()
                        .draft_sealed_compaction(route, named.as_deref(), None, &mut open, &mut seal, device, now)
                        .inspect_err(|error| {
                            tracing::warn!(%error, %app, %record, "sealed record history stays uncompacted");
                            failed
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push((app.clone(), record.clone()));
                        })
                        .ok()?;
                    folded
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((app.clone(), record.clone()));
                    Some(Ok((None, (app, record, named), ops)))
                })
                .collect()
        };
        // The record's history here becomes the compaction; the host's writes it holds only
        // inside it now are what this device knows of the host's history beside it.
        let admit = |live: &mut StatePeer,
                     publication: &mut StoredPublication,
                     (app, record, named): (String, String, Option<String>),
                     ops| {
            compacted_here(
                live,
                publication,
                runs,
                ((&app, &record), named.as_deref()),
                ops,
                now,
            )
        };
        let compacted = self
            .commit_outgoing(workspace, signer, now, draft, admit)
            .await?;
        let failed = failed
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let folded = folded
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A record that folded is tried again when it is next due, not kept resting; one that
        // could not fold rests a while, still due (it may be full) but not read again each pass.
        for (app, record) in folded {
            self.0.resting.folded(workspace, (app, record));
        }
        for (app, record) in failed {
            self.0.resting.failed(workspace, (app, record));
        }
        Ok(compacted)
    }

    /// Draft the compaction ownership of `workspace` sends next, against the history its host
    /// acknowledged: every write ownership held before it is answered. It takes the place and
    /// sequence of the compaction this device made of its own history, and the record's own
    /// history takes it too where it was that compaction. One the host holds nothing for, or
    /// that cannot be drafted, is let go.
    async fn draft_next(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        keys: &dyn SealedHistoryKeys,
        now: SystemTime,
    ) -> Result<(), String> {
        let admissions = self.ownership(workspace);
        let next =
            self.with_current(workspace, |view| next_drafted(view, admissions, keys, now))?;
        let Some((id, message, drafted)) = next else {
            return Ok(());
        };
        let Some((op, history)) = drafted else {
            return self.drop_slot(workspace, id);
        };
        let NeighbourMsg::AppDocument {
            app_id,
            document_id,
            ..
        } = &message
        else {
            return Err("a compaction held is a document write".into());
        };
        let route = (app_id.as_str(), document_id.as_str());
        let draft = |_: &NativeCommittedView| Ok(vec![(Some(id), message.clone(), op)]);
        // `op` is the compaction and the segments it names.
        let admit = |live: &mut StatePeer,
                     publication: &mut StoredPublication,
                     message: NeighbourMsg,
                     ops: Vec<SignedOp>| {
            drafted_here(
                live,
                publication,
                admissions,
                (route, &history),
                message,
                ops,
                now,
            )
        };
        // A compaction that cannot go as drafted is let go: ownership never waits on it.
        match self
            .commit_outgoing(workspace, signer, now, draft, admit)
            .await
        {
            Ok(1..) => Ok(()),
            Ok(0) => self.drop_slot(workspace, id),
            Err(error) => {
                tracing::warn!(%error, "a compaction stays unsent");
                self.drop_slot(workspace, id)
            }
        }
    }

    /// Let go of `id`, a compaction ownership holds the place of: ownership never waits on one
    /// that cannot go.
    fn drop_slot(&self, workspace: WorkspaceId, id: [u8; 32]) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let (changed, mut record) = candidate.change(|_, record| {
            let publication = record.publication.as_mut().ok_or("publication absent")?;
            if !publication.slots.contains(&id) {
                return Ok(false);
            }
            publication.slots.retain(|slot| *slot != id);
            publication.entries.retain(|entry| entry.id != id);
            publication.revision = publication
                .revision
                .checked_add(1)
                .ok_or("native publication revision exhausted")?;
            Ok(true)
        })?;
        if !changed {
            return Ok(());
        }
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_document(workspace, &candidate, record, None)
    }

    /// Commit the writes `draft` drafts of `workspace`, each with the ownership entry it replaces,
    /// into the outgoing ownership in one durable commit of a signed turn of `signer`, this device's
    /// ownership of its key: what the first run drafted is kept, and signed between the runs. A
    /// write `admit` refuses, or one whose entry left ownership meanwhile, stays out. A write that
    /// replaces a compaction ownership holds the place of takes that place; any other goes behind
    /// every write ownership holds, and a compaction keeps its place there until it is drafted.
    /// Answers how many writes were taken.
    ///
    /// # Errors
    /// `signer` is not the enrolled device, it may no longer write, the workspace is fenced,
    /// signing failed, or what was taken cannot be committed.
    async fn commit_outgoing<T: Clone>(
        &self,
        workspace: WorkspaceId,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
        draft: impl FnOnce(
            &NativeCommittedView,
        ) -> Result<Vec<(Option<[u8; 32]>, T, Vec<SignedOp>)>, String>,
        mut admit: impl FnMut(
            &mut StatePeer,
            &mut StoredPublication,
            T,
            Vec<SignedOp>,
        ) -> Result<NeighbourMsg, String>,
    ) -> Result<usize, String> {
        let mut draft = Some(draft);
        let mut drafted = None;
        crate::owner::write::signed_turn(signer, |signed| {
            let _turn = self.0.mutation.lock();
            let candidate = self.view(workspace)?;
            let device = candidate.device().device_public_key;
            if signed.device_public_key() != device {
                return Err("native outgoing signer differs from selected device".into());
            }
            // Nothing to write asks no authority: a workspace whose governance has not arrived
            // (one the host replays only once it is opened) holds no write of this device's.
            let drafted = crate::owner::write::drafted_once(&mut drafted, &mut draft, &candidate)?;
            if drafted.is_empty() {
                return Ok(0);
            }
            candidate
                .peer
                .permissions(device, now)
                .map_err(|error| error.to_string())?;
            let mut writes = Vec::with_capacity(drafted.len());
            for (replaces, with, ops) in drafted.iter() {
                let mut signed_ops = Vec::with_capacity(ops.len());
                for op in ops {
                    let mut op = op.clone();
                    collab_types::sign_op(&mut op, signed).map_err(|error| error.to_string())?;
                    signed_ops.push(op);
                }
                writes.push((*replaces, with.clone(), signed_ops));
            }
            signed.settled()?;
            let (taken, mut record) = candidate.change(|live, record| {
                let mut taken = 0usize;
                for (replaces, with, ops) in writes {
                    let publication = record.publication.as_mut().ok_or("publication absent")?;
                    if replaces
                        .is_some_and(|id| !publication.entries.iter().any(|kept| kept.id == id))
                    {
                        continue;
                    }
                    let compaction = ops.first().is_some_and(|op| op.kind == OpKind::Compaction);
                    let segments: Vec<SignedOp> = ops.iter().skip(1).cloned().collect();
                    // Admitted on copies: a write that cannot be committed as ownership entries
                    // leaves the replica and ownership as they were. A compaction in segments is
                    // admitted whole, and its segments go right behind it.
                    let mut attempt = live.clone();
                    let mut kept = publication.clone();
                    let admitted = (|| {
                        let message = admit(&mut attempt, &mut kept, with, ops)?;
                        let bytes = encode(&message, MAX_BYTES)?;
                        let segments = segments
                            .into_iter()
                            .map(|segment| encode(&with_envelope(&message, segment), MAX_BYTES))
                            .collect::<Result<Vec<_>, String>>()?;
                        Ok::<_, String>((bytes, segments))
                    })();
                    match admitted {
                        Ok((bytes, segments)) => {
                            *publication = kept;
                            *live = attempt;
                            let id = *blake3::hash(&bytes).as_bytes();
                            match replaces {
                                Some(slot) if publication.slots.contains(&slot) => {
                                    in_place(record, slot, bytes)?;
                                }
                                _ => {
                                    publication.entries.retain(|kept| Some(kept.id) != replaces);
                                    append(record, bytes)?;
                                    if compaction {
                                        record
                                            .publication
                                            .as_mut()
                                            .ok_or("publication absent")?
                                            .slots
                                            .push(id);
                                    }
                                }
                            }
                            let mut after = id;
                            for segment in segments {
                                after = append_after(record, after, segment)?;
                            }
                            taken = taken.saturating_add(1);
                        }
                        Err(error) => tracing::warn!(%error, "an outgoing write stays as it was"),
                    }
                }
                Ok(taken)
            })?;
            if taken == 0 {
                return Ok(0);
            }
            record.generation = record
                .generation
                .checked_add(1)
                .ok_or("native owner generation exhausted")?;
            self.publish_document(workspace, &candidate, record, None)?;
            Ok(taken)
        })
        .await
    }

    /// Take `message` into `publication` when it is a compaction the host holds of a record this
    /// device compacted ahead of the host: the record's history here does not line up with it,
    /// so it goes into what this device knows of the host's history, and the record's history
    /// takes it once it holds everything the record's own compaction folded. `None` for any
    /// other input, which is admitted as it is.
    pub(crate) fn receive_hosted(
        &self,
        candidate: &NativeCommittedView,
        publication: &mut StoredPublication,
        message: &NeighbourMsg,
        now: SystemTime,
    ) -> Option<Result<crate::AdmissionReport, String>> {
        let NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return None;
        };
        let route = (app_id.as_str(), document_id.as_str());
        if envelope.kind != OpKind::Compaction
            || !publication
                .hosted
                .iter()
                .any(|held| held.app == route.0 && held.record == route.1)
        {
            return None;
        }
        let runs = self.ownership(candidate.record.workspace);
        let mut live = candidate.live();
        // A compaction in segments is taken once all of its pieces are here.
        let gathered;
        let envelope = if collab_types::is_compaction_piece(envelope) {
            match live
                .publication_mut()
                .gather_sealed_piece(route, (**envelope).clone())
            {
                Err(error) => return Some(Err(error)),
                Ok(None) => {
                    return Some(Ok(crate::AdmissionReport::new(
                        crate::InputDisposition::MetadataOnly,
                        crate::ReceiveOutcome::default(),
                    )));
                }
                Ok(Some(head)) => {
                    gathered = head;
                    &gathered
                }
            }
        } else {
            &**envelope
        };
        let taken = (|| {
            live.publication()
                .check_sealed_compaction(route, envelope, now)?;
            let owed = owed_to(publication, runs, route)?;
            let mut peer = live.clone();
            let mut next = publication.clone();
            // This device's compactions the host took before this one, whose answers are still
            // on their way.
            for sent in owed.iter().filter(|sent| {
                sent.kind == OpKind::Compaction
                    && !collab_types::is_compaction_segment(sent)
                    && covers(envelope, sent)
            }) {
                take_hosted(&mut peer, &mut next, runs, route, sent, &owed, now)?;
            }
            take_hosted(&mut peer, &mut next, runs, route, envelope, &owed, now)?;
            let admission = crate::admission::DocumentAdmission::from_op(
                cluster_id, route.0, route.1, envelope,
            )
            .map_err(|error| error.to_string())?;
            *live = peer;
            *publication = next;
            Ok(crate::AdmissionReport::new(
                crate::InputDisposition::Accepted(admission),
                crate::ReceiveOutcome {
                    changed: true,
                    metadata_changed: true,
                    ..Default::default()
                },
            ))
        })();
        Some(taken)
    }
}
/// Commit `op`, a compaction of this device's own history of `app`'s document, into it: its
/// history becomes the compaction, and the server's writes it holds only inside it now are what
/// this device knows of the server's history beside it.
fn compacted_here(
    live: &mut StatePeer,
    publication: &mut StoredPublication,
    runs: Ownership<'_>,
    ((app, record), named): ((&str, &str), Option<&str>),
    ops: Vec<SignedOp>,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    let history = host_history(live, publication, runs, (app, record))?;
    let mut hosted = if let Some(hosted) = publication.hosted(runs, (app, record))? {
        hosted
    } else {
        // The record's history was the host's: its compaction, if any, is first.
        let mut hosted = Hosted::default();
        if let Some(first) = history
            .first()
            .filter(|first| first.kind == OpKind::Compaction)
        {
            hosted.take(first)?;
        }
        hosted
    };
    let unanswered = ops.first().cloned();
    let message = live
        .publication_mut()
        .admit_sealed_compaction(app, record, named, ops, now)?
        .into_iter()
        .next()
        .ok_or("a compaction publishes as its own message")?;
    if let Some(op) = &unanswered {
        publication.unanswered_write(op);
    }
    hosted.writes = history
        .into_iter()
        .filter(|write| !live.publication().holds_sealed(app, record, write))
        .collect();
    publication.set_hosted(runs, (app, record), Some(&hosted))?;
    Ok(message)
}
/// The compaction ownership of `view` sends next, when ownership holds its place at its head:
/// drafted against `host_history`, the server's history as it acknowledged it, as ownership's own
/// message, or `None` when it cannot be drafted.
type NextDrafted = (
    [u8; 32],
    NeighbourMsg,
    Option<(Vec<SignedOp>, Vec<SignedOp>)>,
);
fn next_drafted(
    view: &NativeCommittedView,
    runs: Ownership<'_>,
    keys: &dyn SealedHistoryKeys,
    now: SystemTime,
) -> Result<Option<NextDrafted>, String> {
    let workspace = view.record.workspace;
    let publication = state(view)?;
    let Some(entry) = publication
        .entries
        .first()
        .filter(|entry| publication.slots.contains(&entry.id))
    else {
        return Ok(None);
    };
    let message = NeighbourMsg::decode(&entry.bytes).map_err(|e| e.to_string())?;
    let NeighbourMsg::AppDocument {
        app_id,
        document_id,
        readers,
        ..
    } = &message
    else {
        return Err("a compaction held is a document write".into());
    };
    let route = (app_id.as_str(), document_id.as_str());
    let history = host_history(&view.peer, publication, runs, route)?;
    // Numbered as the last of this device's writes the history covers, as a compaction is: the
    // seq stored in the entry belongs to the draft its history once held, which may be longer.
    let device = view.device().device_public_key;
    let seq = history
        .iter()
        .filter(|write| write.author.as_bytes() == &device)
        .map(|write| write.seq)
        .max()
        .unwrap_or(0);
    let named = readers.as_deref();
    let group = collab_types::reader_group(workspace, named.unwrap_or(collab_types::EVERY_MEMBER))
        .ok_or("invalid reader set for compaction")?;
    let mut open = |write: &SignedOp| keys.open(group, write);
    let mut seal = |snapshot: &[u8]| {
        seal_for_document(keys, &view.peer, workspace, route, named, snapshot, now)
    };
    let drafted = view.peer.publication().draft_sealed_compaction_of(
        route,
        named,
        history.clone(),
        &mut open,
        &mut seal,
        (device, seq),
        now,
    );
    let drafted = drafted
        .inspect_err(|error| tracing::warn!(%error, "a compaction stays unsent"))
        .ok();
    Ok(Some((entry.id, message, drafted.map(|ops| (ops, history)))))
}
/// Commit `op`, drafted against `history` (the server's history of `app`'s document as it
/// acknowledged it) in place of `message`, ownership's compaction of this device's own history:
/// where its history is that compaction, it takes `op` in its place. Answers the
/// ownership message `op` goes as.
fn drafted_here(
    live: &mut StatePeer,
    publication: &mut StoredPublication,
    runs: Ownership<'_>,
    (route, history): ((&str, &str), &[SignedOp]),
    message: NeighbourMsg,
    ops: Vec<SignedOp>,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    let mut ops = ops.into_iter();
    let op = ops.next().ok_or("no compaction drafted")?;
    for segment in ops {
        live.publication_mut().gather_sealed_piece(route, segment)?;
    }
    let NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        document_id,
        readers,
        ..
    } = message
    else {
        return Err("a compaction held is a document write".into());
    };
    let first = live
        .publication()
        .sealed_history(route.0, route.1)
        .into_iter()
        .next();
    // A compaction of this device's the document holds first is one its server has not taken,
    // or has: one it has not is what this draft replaces, whether the entry held for it is
    // that compaction or one drafted there before it.
    let replaced = first
        .as_ref()
        .filter(|first| !collab_types::is_compaction_segment(first))
        .filter(|first| first.kind == OpKind::Compaction)
        .filter(|first| publication.unanswered(first))
        .cloned();
    if let Some(replaced) = replaced {
        let mut hosted = publication.hosted(runs, route)?.unwrap_or_default();
        live.publication_mut().take_sealed_compaction(
            route,
            &op,
            &|write| hosted.covers(write),
            now,
        )?;
        publication.answered_write(&replaced);
        hosted.writes = history
            .iter()
            .filter(|write| !live.publication().holds_sealed(route.0, route.1, write))
            .cloned()
            .collect();
        publication.set_hosted(runs, route, Some(&hosted))?;
    } else {
        live.publication_mut()
            .record_sealed_compaction(route.0, route.1, &op)?;
    }
    publication.unanswered_write(&op);
    Ok(NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        document_id,
        envelope: Box::new(op),
        readers,
    })
}
/// `message` carrying `envelope`, a segment of the compaction it carries, in its place.
fn with_envelope(message: &NeighbourMsg, envelope: SignedOp) -> NeighbourMsg {
    let mut message = message.clone();
    if let NeighbourMsg::AppDocument { envelope: held, .. } = &mut message {
        **held = envelope;
    }
    message
}
/// Commit `bytes` into ownership right behind the entry `after`, as the next segment of the
/// compaction there, and answer its own entry's id. Behind it a store keeps a segment in the
/// tail of the compaction it continues.
fn append_after(
    view: &mut stored::StoredWorkspace,
    after: [u8; 32],
    bytes: Vec<u8>,
) -> Result<[u8; 32], String> {
    let id = *blake3::hash(&bytes).as_bytes();
    append(view, bytes)?;
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    if let (Some(at), Some(from)) = (
        publication
            .entries
            .iter()
            .position(|entry| entry.id == after),
        publication.entries.iter().position(|entry| entry.id == id),
    ) && from > at.saturating_add(1)
    {
        let entry = publication.entries.remove(from);
        publication.entries.insert(at.saturating_add(1), entry);
    }
    Ok(id)
}
/// Put `bytes`, a compaction drafted for the host, where the compaction ownership held the
/// place of as `id` stands: it goes in that compaction's turn.
fn in_place(
    view: &mut stored::StoredWorkspace,
    id: [u8; 32],
    bytes: Vec<u8>,
) -> Result<(), String> {
    let message = NeighbourMsg::decode(&bytes).map_err(|e| e.to_string())?;
    let (timestamp_ns, channel, incremental) = metadata(&message)?;
    let binding = request(&message)?.binding()?;
    let publication = view.publication.as_mut().ok_or("publication absent")?;
    let entry = publication
        .entries
        .iter_mut()
        .find(|entry| entry.id == id)
        .ok_or("the compaction ownership held left it")?;
    *entry = StoredOutgoing {
        ordinal: entry.ordinal,
        id: *blake3::hash(&bytes).as_bytes(),
        bytes,
        binding,
        timestamp_ns,
        channel,
        incremental,
    };
    publication.slots.retain(|slot| *slot != id);
    publication.revision = publication
        .revision
        .checked_add(1)
        .ok_or("native publication revision exhausted")?;
    Ok(())
}
/// Whether `entry` of ownership is a compaction.
fn is_compaction(entry: &StoredOutgoing) -> bool {
    matches!(NeighbourMsg::decode(&entry.bytes),
        Ok(NeighbourMsg::AppDocument { envelope, .. })
            if envelope.kind == OpKind::Compaction && !collab_types::is_compaction_segment(&envelope))
}
/// Whether `compaction` covers `write`.
fn covers(compaction: &SignedOp, write: &SignedOp) -> bool {
    compaction.kind == OpKind::Compaction
        && collab_types::SealedCompaction::parse(compaction)
            .is_ok_and(|wrapper| wrapper.covers(write))
}
/// This device's writes of `record` of `app` its ownership still owes the host, in the order it
/// sends them.
fn owed_to(
    publication: &StoredPublication,
    runs: Ownership<'_>,
    (app, record): (&str, &str),
) -> Result<Vec<SignedOp>, String> {
    let spilled = spilled_writes(publication, runs)?;
    Ok(publication
        .entries
        .iter()
        .chain(&spilled)
        .chain(&publication.tail)
        .filter_map(|entry| match NeighbourMsg::decode(&entry.bytes) {
            Ok(NeighbourMsg::AppDocument {
                app_id,
                document_id,
                envelope,
                ..
            }) if app_id == app && document_id == record => Some(*envelope),
            _ => None,
        })
        .collect())
}
/// The history the server holds of `app`'s document as far as it acknowledged it: what this
/// device holds of it but the writes its server has not taken.
fn host_history(
    live: &StatePeer,
    publication: &StoredPublication,
    runs: Ownership<'_>,
    (app, record): (&str, &str),
) -> Result<Vec<SignedOp>, String> {
    let held = live.publication().sealed_history(app, record);
    let history: Vec<SignedOp> = match publication.hosted(runs, (app, record))? {
        // The record's own compaction is first here, and the host never held it; nor does it
        // hold the writes here a compaction of its own folded.
        Some(hosted) => {
            let rest: Vec<SignedOp> = held
                .into_iter()
                .skip(1)
                .filter(|write| {
                    !hosted.covers(write) && !collab_types::is_compaction_segment(write)
                })
                .collect();
            hosted.writes.into_iter().chain(rest).collect()
        }
        None => held,
    };
    Ok(history
        .into_iter()
        .filter(|write| !publication.unanswered(write))
        .collect())
}
/// Take `compaction`, which the host now holds first in its history of `record` of `app`, into
/// what this device knows of that history. Once that holds nothing beside it and ownership owes
/// none of the writes the record's own compaction folded, the record's history takes it in
/// that compaction's place: it is the host's again. `owed` are the writes ownership still owes.
fn take_hosted(
    live: &mut StatePeer,
    publication: &mut StoredPublication,
    runs: Ownership<'_>,
    route: (&str, &str),
    compaction: &SignedOp,
    owed: &[SignedOp],
    now: SystemTime,
) -> Result<(), String> {
    let held = live.publication().sealed_history(route.0, route.1);
    let first = held.first().map(|first| first.signature);
    let Some(mut hosted) = publication.hosted(runs, route)? else {
        // Taken already: a later compaction of the host's that covers it came first.
        if first != Some(compaction.signature)
            && !held.first().is_some_and(|first| covers(first, compaction))
        {
            live.publication_mut()
                .take_sealed_compaction(route, compaction, &|_| false, now)?;
        }
        return Ok(());
    };
    if hosted.covers(compaction)
        || hosted
            .writes
            .iter()
            .any(|write| write.signature == compaction.signature)
    {
        return Ok(());
    }
    hosted.take(compaction)?;
    let folded = owed.iter().any(|write| {
        write.kind != OpKind::Compaction
            && !held.iter().any(|kept| kept.signature == write.signature)
    });
    if hosted.writes.len() > 1 || folded {
        return publication.set_hosted(runs, route, Some(&hosted));
    }
    if first != Some(compaction.signature) {
        live.publication_mut().take_sealed_compaction(
            route,
            compaction,
            &|write| hosted.covers(write),
            now,
        )?;
    }
    publication.set_hosted(runs, route, None)
}
#[cfg(test)]
#[path = "publication_encoding_tests.rs"]
mod encoding_tests;
#[cfg(test)]
#[path = "publication_tests.rs"]
mod tests;
