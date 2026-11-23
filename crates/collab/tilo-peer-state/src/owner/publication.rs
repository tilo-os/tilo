//! The outgoing ownership an earlier build kept in a workspace's record, read
//! so a record an earlier build wrote still opens and its journal replays.
//!
//! This build keeps no outgoing ownership: a write of this device's is
//! admitted, made durable and sent on a link ([`crate::owner::NativeWorkspaceOwner`]).
//! The types here are the record's layout, byte for byte as that build wrote
//! them, and nothing constructs one.

use super::answers::Unanswered;
use collab_types::SignedOp;
use serde::{Deserialize, Serialize};

/// Most writes a record kept at its head. This crate's tests keep fewer, so a
/// test crosses the bound many times over in the writes it can afford.
#[cfg(not(test))]
const MAX_ENTRIES: usize = 64;
#[cfg(test)]
const MAX_ENTRIES: usize = 8;
const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Bytes of writes one batch of ownership's backlog gathered before the store keeps it.
const RUN_BYTES: usize = 1024 * 1024;
/// Most bytes a batch the store keeps can hold: its writes, then one more that did not fit.
pub(crate) const MAX_RUN_BYTES: usize = RUN_BYTES + MAX_BYTES + 64 * 1024;

/// The name a run of ownership's writes is kept under, by the hash of its bytes.
#[must_use]
pub fn ownership_run_name(workspace: super::WorkspaceId, hash: [u8; 32]) -> String {
    kept_name(workspace, "custody", hash)
}

/// The name builds kept the same writes under for a while.
#[must_use]
pub fn ownership_name_between(workspace: super::WorkspaceId, hash: [u8; 32]) -> String {
    kept_name(workspace, "ownership", hash)
}

fn kept_name(workspace: super::WorkspaceId, kept_as: &str, hash: [u8; 32]) -> String {
    format!(
        "{workspace}.{kept_as}.{}",
        blake3::Hash::from_bytes(hash).to_hex()
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredPublication {
    version: u32,
    revision: u64,
    next_ordinal: u64,
    #[serde(deserialize_with = "read_entries")]
    pub(crate) entries: Vec<StoredOutgoing>,
    /// Recovery projections an earlier build signed for its expiring
    /// frames. Read and then dropped; the field stays in the format so records
    /// that hold one still load.
    #[serde(
        default,
        deserialize_with = "read_entries",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub(crate) projection: Vec<StoredOutgoing>,
    #[serde(default, deserialize_with = "read_entries")]
    governance: Vec<StoredOutgoing>,
    /// What the workspace's server answered to this device's writes, per document.
    #[serde(
        default,
        skip_serializing_if = "Answered::is_empty",
        with = "answered_list"
    )]
    answered: Answered,
    /// Writes behind `entries` the store keeps beside the record, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    spilled: Vec<SpilledRun>,
    /// The newest writes behind `spilled`.
    #[serde(
        default,
        deserialize_with = "read_entries",
        skip_serializing_if = "Vec::is_empty"
    )]
    tail: Vec<StoredOutgoing>,
    /// The sealed records whose history an earlier build's compaction folded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hosted: Vec<HostedHistory>,
    /// Ownership's compactions not drafted yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    slots: Vec<[u8; 32]>,
    /// This device's writes its server has not taken, by signature
    /// ([`super::answers`]).
    #[serde(
        default,
        skip_serializing_if = "Unanswered::is_empty",
        with = "super::answers::unanswered_list"
    )]
    pub(super) unanswered: Unanswered,
}

/// One of [`StoredPublication::hosted`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HostedHistory {
    app: String,
    record: String,
    #[serde(default)]
    runs: Vec<[u8; 32]>,
    #[serde(default)]
    writes: Vec<HostedWrite>,
    #[serde(default)]
    covered: Vec<([u8; 32], u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    dead_code,
    reason = "the record's layout an earlier build wrote; read, never built"
)]
struct HostedWrite(#[serde(with = "crate::owner::stored::base64_bytes")] Vec<u8>);

/// Writes of ownership's backlog the store kept as one run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SpilledRun {
    pub(crate) hash: [u8; 32],
    pub(crate) writes: u32,
    pub(crate) taken: u32,
}

/// The server's answer to this device's writes to one record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Answer {
    durable: u64,
    refused: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_order: Option<u64>,
}

/// The server's answers, by app and document.
pub(crate) type Answered = crate::ordered::Ordered<(String, String), Answer>;

/// [`Answered`] as the list the workspace's file holds.
mod answered_list {
    use super::Answered;
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
                let answer = super::Answer {
                    durable: held.durable,
                    refused: held.refused,
                    last_order: held.last_order,
                };
                ((held.app, held.record), answer)
            })
            .collect())
    }
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
    /// Base64 text in the owner's JSON files; a binary encoding keeps its
    /// sequence.
    #[serde(
        serialize_with = "crate::owner::stored::base64_bytes::serialize",
        deserialize_with = "read_message_bytes"
    )]
    pub(crate) bytes: Vec<u8>,
    binding: RecordBinding,
    timestamp_ns: u64,
    channel: [u8; 32],
    incremental: bool,
}

/// A write's binding as an earlier build recorded it
/// (`owner_commit::CommitBinding`), kept byte for byte so a journal that
/// encodes one still replays. The variants stay at their encoded positions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RecordBinding {
    workspace: String,
    target: RecordTarget,
    request_digest: [u8; 32],
    basis: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    dead_code,
    reason = "the record's layout an earlier build wrote; read, never built"
)]
enum RecordTarget {
    Document {
        app: String,
        record: String,
    },
    EffectReceipt {
        invocation: uuid::Uuid,
    },
    DesktopCommand {
        invocation: uuid::Uuid,
    },
    RefusedAgentRuntime,
    DocumentImport {
        invocation: uuid::Uuid,
    },
    RefusedWorkspaceTask,
    WorkspaceMembershipCommand {
        device: [u8; 32],
        invocation: uuid::Uuid,
    },
    NativeCheckpointRoot {
        root: [u8; 32],
    },
    RefusedShortcutChange {
        device: [u8; 32],
        invocation: uuid::Uuid,
    },
    JoinAdmission {
        device: [u8; 32],
    },
    RefusedDirectPair,
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

impl StoredPublication {
    /// What a journal entry carries apart from the rest of the workspace's state
    /// ([`super::journal`]): the server's answers, the writes it has not taken, and the lists of
    /// writes ownership held (its governance, its head and its tail), all taken out of this.
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
