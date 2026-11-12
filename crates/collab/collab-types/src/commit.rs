//! One action, one workspace commit.
//!
//! A workspace's state is split into storage units: one Automerge document per
//! app and record, a chat's parts, a host's records. An action can write more
//! than one of them (creating a child writes the child's record and the
//! parent's reference to it), and publishing each unit on its own lets a peer
//! see half an action and leaves history without a name for the whole of it.
//!
//! A [`WorkspaceCommit`] is that name. It lists every unit the action wrote —
//! the app and record, and the hash of each change it made there — under one
//! author, one time, and the invocation and run it was made in. A part's
//! document frame carries the commit inside its signed payload
//! ([`WorkspaceCommit::frame`]), so the device signature over that payload
//! signs the commit, and a receiver reads it off the envelope that delivered
//! the part ([`WorkspaceCommit::of_envelope`]).
//!
//! The commit groups; it does not describe. Every change it names carries the
//! action that made it in its own history entry ([`tilo_actions::ActionContext`]):
//! that entry is what a grant is checked against, what replay verifies, and
//! what undo re-runs. The commit duplicates none of it — where the changes are,
//! so are the action's name, its resources, and the fields it wrote.
//!
//! The commit is a record kind of its own ([`COMMIT_RECORD_KIND`], version
//! [`COMMIT_VERSION`]), so a host can store it beside the units it names. The
//! units, their channels and their seals keep their names: only the frame
//! around a document frame is new, and a frame without one is read exactly as
//! before. Version 1 commits still decode (they carried the action's name, item
//! and invocation); version 2 dropped the name and item for the invocation and
//! run alone.

use serde::{Deserialize, Serialize};

use crate::SignedOp;
use crate::claim;
use crate::op::DocumentMutation;

/// The record kind of a workspace commit.
pub const COMMIT_RECORD_KIND: &str = "tilo.workspace.commit";
/// Version of the commit object; a receiver refuses one it does not know.
pub const COMMIT_VERSION: u16 = 2;
/// The commit version before a commit was grouping alone: it named the
/// action's name and item. Read, never written.
pub const COMMIT_VERSION_NAMED_ACTION: u16 = 1;
/// Prefix of a document frame that carries an action's commit: the commit's
/// byte length, the commit, then the document frame it belongs to.
pub const COMMIT_FRAME_PREFIX: &[u8] = b"tilo-commit-frame-v1\0";
/// Largest commit a receiver accepts, which bounds the bytes one framed
/// payload adds to a document frame beyond its mutation budget.
pub const MAX_COMMIT_BYTES: usize = 256 * 1024;

/// Who made an action: the authenticated principal that invoked it, and the
/// device that signed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitAuthor {
    /// The principal the action was invoked as (`alice`), when the action
    /// came through a door that admitted one. `None` for a change nothing
    /// but its device accounts for.
    pub principal: Option<String>,
    /// The device key that signed the part carrying this commit. Every part
    /// of one commit is signed by this key, and a receiver refuses a part
    /// any other device signed.
    pub device: [u8; 32],
}

/// A run a group of actions belongs to: what every change of one agent run,
/// one task or one command line shares.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitGroup {
    /// The run's id, shared by every change it made.
    pub id: String,
    /// Its label, as history names it.
    pub label: String,
    /// Its kind (`agent-run`, `task-run`, `command-run`, …).
    pub kind: String,
}

/// One unit an action wrote: which app's record, and the hashes of the
/// changes it made there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitPart {
    /// The app whose record the unit belongs to.
    pub app: String,
    /// The record the unit belongs to; empty for the app's route without a
    /// record (one document per app and workspace).
    pub record: String,
    /// The hash of every change the action made in this unit, in the order it
    /// made them.
    pub changes: Vec<[u8; 32]>,
}

impl CommitPart {
    /// Whether this part is the unit of `app`'s `record`.
    #[must_use]
    pub fn is(&self, app: &str, record: &str) -> bool {
        self.app == app && self.record == record
    }
}

/// One unit and what changed in it, as the publisher of a pass holds it.
#[derive(Clone, Copy)]
pub struct UnitMutation<'a> {
    /// The app whose record the unit belongs to.
    pub app: &'a str,
    /// The record the unit belongs to.
    pub record: &'a str,
    /// What changed in the unit during the pass.
    pub mutation: &'a DocumentMutation,
    /// The unit's document, holding the mutation's changes, when the
    /// publisher has it. The next letter of a typed word continues its
    /// gesture's first change and names its action only through it, which an
    /// earlier pass may have carried.
    pub document: Option<&'a automerge::Automerge>,
}

/// One signed action across every unit it wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCommit {
    /// [`COMMIT_RECORD_KIND`] — what this record is.
    pub kind: String,
    /// [`COMMIT_VERSION`] — how to read it.
    pub version: u16,
    /// The workspace whose units the action wrote.
    pub workspace: String,
    /// Who made it: the principal and the device that signed it.
    pub author: CommitAuthor,
    /// When the action was made, in milliseconds since the Unix epoch, as the
    /// author's device read the clock.
    pub at_ms: u64,
    /// The invocation the action ran under, when an admitted door ran it.
    pub invocation: Option<uuid::Uuid>,
    /// The run the action was invoked in, when it was invoked in one: an
    /// agent run, a task, a command line. Undo takes a run's steps back as
    /// one.
    pub run: Option<CommitGroup>,
    /// Every unit it wrote. A receiver applies all of them or none.
    pub parts: Vec<CommitPart>,
}

/// Why a commit cannot be read or built.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CommitError {
    /// The value did not encode.
    #[error("workspace commit does not encode: {0}")]
    Encode(String),
    /// The bytes are not a commit.
    #[error("workspace commit does not decode: {0}")]
    Decode(String),
    /// The record kind is not [`COMMIT_RECORD_KIND`].
    #[error("not a workspace commit: kind {0:?}")]
    Kind(String),
    /// The version is not [`COMMIT_VERSION`].
    #[error("workspace commit version {0} is not supported")]
    Version(u16),
    /// The commit names no unit, so it commits to nothing.
    #[error("workspace commit names no part")]
    NoParts,
    /// Two parts name the same unit.
    #[error("workspace commit names {app:?}/{record:?} twice")]
    DuplicatePart { app: String, record: String },
    /// A part names no change.
    #[error("workspace commit part {app:?}/{record:?} names no change")]
    EmptyPart { app: String, record: String },
    /// The commit is larger than [`MAX_COMMIT_BYTES`].
    #[error("workspace commit is {actual} bytes; maximum is {max}")]
    TooLarge { actual: usize, max: usize },
    /// A change the commit covers names no action.
    #[error("a change the action made names no action: {0}")]
    Unnamed(String),
    /// The bytes are not a framed document payload: the frame's length does
    /// not fit the bytes that follow it.
    #[error("commit frame is truncated")]
    Truncated,
    /// The envelope's device is not the commit's author.
    #[error("the carrying envelope is signed by a device the commit does not name")]
    ForeignDevice,
}

impl WorkspaceCommit {
    /// A commit of `parts`, made by `author` at `at_ms`, for the action that
    /// ran under `invocation` in `run`, in `workspace`.
    #[must_use]
    pub fn new(
        workspace: impl Into<String>,
        author: CommitAuthor,
        at_ms: u64,
        invocation: Option<uuid::Uuid>,
        run: Option<CommitGroup>,
        parts: Vec<CommitPart>,
    ) -> Self {
        Self {
            kind: COMMIT_RECORD_KIND.to_owned(),
            version: COMMIT_VERSION,
            workspace: workspace.into(),
            author,
            at_ms,
            invocation,
            run,
            parts,
        }
    }

    /// The commit for one action that published `mutation` as the unit of
    /// `app`'s `record`: the hash of every change it carries, the time the
    /// latest of them was made, and the invocation and run they agree on.
    /// `None` for a mutation that carries no change (a snapshot, or an empty
    /// set).
    ///
    /// # Errors
    /// [`CommitError::Unnamed`] for a change that names no action.
    pub fn of_mutation(
        workspace: &str,
        app: &str,
        record: &str,
        author: &CommitAuthor,
        mutation: &DocumentMutation,
    ) -> Result<Option<Self>, CommitError> {
        let unit = UnitMutation {
            app,
            record,
            mutation,
            document: None,
        };
        Ok(
            Self::of_units(workspace, author, std::slice::from_ref(&unit))?
                .into_iter()
                .next()
                .flatten(),
        )
    }

    /// The commit each unit of one publication pass travels under: one entry
    /// per entry of `units`, in that order. `None` for a unit that carries no
    /// change (a snapshot, or an empty set).
    ///
    /// The units one action wrote share one commit, which names every one of
    /// them as a part, so a peer applies all of the action or none. Units
    /// belong to one action when every change of each ran under the same
    /// invocation; a unit whose changes ran under several invocations, or
    /// none, travels under a commit of its own.
    ///
    /// `author` names who publishes. Its `device` may be left as zeros by a
    /// caller that holds no signing key: the transport signs the frame and
    /// stamps the device it signs with.
    ///
    /// # Errors
    /// [`CommitError::Unnamed`] for a change that names no action.
    pub fn of_units(
        workspace: &str,
        author: &CommitAuthor,
        units: &[UnitMutation<'_>],
    ) -> Result<Vec<Option<Self>>, CommitError> {
        let read: Vec<Option<Read>> = units.iter().map(Read::of).collect::<Result<Vec<_>, _>>()?;
        // The units of one action, by the invocation they ran under: the
        // change each of them made names it, so two units a person's one
        // action wrote are one action here. A unit that ran under none, or
        // under several, is an action of its own.
        let mut groups: Vec<(Option<uuid::Uuid>, Vec<usize>)> = Vec::new();
        for (index, unit) in read.iter().enumerate() {
            let Some(signature) = unit.as_ref().and_then(|unit| unit.signature) else {
                if unit.is_some() {
                    groups.push((None, vec![index]));
                }
                continue;
            };
            match groups
                .iter_mut()
                .find(|(known, _)| *known == Some(signature))
            {
                Some((_, group)) => group.push(index),
                None => groups.push((Some(signature), vec![index])),
            }
        }
        let mut commits: Vec<Option<Self>> = vec![None; read.len()];
        for (invocation, group) in groups {
            let parts: Vec<&Read> = group
                .iter()
                .filter_map(|index| read.get(*index).and_then(Option::as_ref))
                .collect();
            let Some(first) = parts.first() else {
                continue;
            };
            let commit = Self::of_parts(workspace, author, invocation, first, &parts);
            for index in group {
                if let Some(slot) = commits.get_mut(index) {
                    *slot = Some(commit.clone());
                }
            }
        }
        Ok(commits)
    }

    /// The commit that names `parts` as the action that ran under
    /// `invocation`.
    fn of_parts(
        workspace: &str,
        author: &CommitAuthor,
        invocation: Option<uuid::Uuid>,
        first: &Read,
        parts: &[&Read],
    ) -> Self {
        let principal = parts
            .iter()
            .find_map(|part| part.principal.clone())
            .or_else(|| author.principal.clone());
        // The author's own change times, so every peer reads one time for the
        // action and a restarted one reads the same.
        let at_ms = parts.iter().map(|part| part.at_ms).max().unwrap_or(0);
        Self::new(
            workspace,
            CommitAuthor {
                principal,
                device: author.device,
            },
            at_ms,
            invocation,
            first.run.clone(),
            parts.iter().map(|part| part.part.clone()).collect(),
        )
    }

    /// The commit's canonical bytes: what a host stores, and what its id
    /// addresses.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        #[allow(
            clippy::expect_used,
            reason = "a commit of strings, lists and fixed arrays always encodes"
        )]
        crate::canonical::canonical_bytes(self).expect("a workspace commit encodes")
    }

    /// The commit of `bytes`, checked: a known kind and version, at least one
    /// part, no unit twice, and a change in every part.
    ///
    /// # Errors
    /// [`CommitError`] for bytes that are not such a commit.
    pub fn decode(bytes: &[u8]) -> Result<Self, CommitError> {
        // The version is the second field of every commit, so it is read
        // before the rest: version 1 carried the action's name and item where
        // this version carries the invocation and run, and postcard cannot
        // tell a missing field from a short one.
        let (head, _) = postcard::take_from_bytes::<Head>(bytes)
            .map_err(|error| CommitError::Decode(error.to_string()))?;
        let commit = match head.version {
            COMMIT_VERSION => crate::canonical::from_canonical_bytes::<Self>(bytes)
                .map_err(|error| CommitError::Decode(error.to_string()))?,
            COMMIT_VERSION_NAMED_ACTION => {
                let old = crate::canonical::from_canonical_bytes::<V1>(bytes)
                    .map_err(|error| CommitError::Decode(error.to_string()))?;
                Self::of_v1(old)
            }
            other => return Err(CommitError::Version(other)),
        };
        commit.check()?;
        Ok(commit)
    }

    /// The commit `old` names, as this version reads it: the invocation it
    /// ran under, and no run (version 1 carried none). Its action's name and
    /// item are dropped: the changes carry them.
    fn of_v1(old: V1) -> Self {
        Self {
            kind: old.kind,
            version: COMMIT_VERSION,
            workspace: old.workspace,
            author: old.author,
            at_ms: old.at_ms,
            invocation: old.action.invocation,
            run: None,
            parts: old
                .parts
                .into_iter()
                .map(|part| CommitPart {
                    app: part.app,
                    record: part.record,
                    changes: part.changes,
                })
                .collect(),
        }
    }

    /// The commit's content address: two peers holding the same commit name
    /// it by the same bytes.
    #[must_use]
    pub fn id(&self) -> [u8; 32] {
        *blake3::hash(&self.encode()).as_bytes()
    }

    /// Whether every part is well formed.
    ///
    /// # Errors
    /// [`CommitError::Kind`], [`Version`](CommitError::Version),
    /// [`NoParts`](CommitError::NoParts),
    /// [`DuplicatePart`](CommitError::DuplicatePart) or
    /// [`EmptyPart`](CommitError::EmptyPart).
    pub fn check(&self) -> Result<(), CommitError> {
        if self.kind != COMMIT_RECORD_KIND {
            return Err(CommitError::Kind(self.kind.clone()));
        }
        if self.version != COMMIT_VERSION {
            return Err(CommitError::Version(self.version));
        }
        if self.parts.is_empty() {
            return Err(CommitError::NoParts);
        }
        for (index, part) in self.parts.iter().enumerate() {
            if part.changes.is_empty() {
                return Err(CommitError::EmptyPart {
                    app: part.app.clone(),
                    record: part.record.clone(),
                });
            }
            if self
                .parts
                .iter()
                .take(index)
                .any(|earlier| earlier.is(&part.app, &part.record))
            {
                return Err(CommitError::DuplicatePart {
                    app: part.app.clone(),
                    record: part.record.clone(),
                });
            }
        }
        Ok(())
    }

    /// The part of `app`'s `record`, when the action wrote it.
    #[must_use]
    pub fn part(&self, app: &str, record: &str) -> Option<&CommitPart> {
        self.parts.iter().find(|part| part.is(app, record))
    }

    /// Whether the action wrote the unit of `app`'s `record`.
    #[must_use]
    pub fn covers(&self, app: &str, record: &str) -> bool {
        self.part(app, record).is_some()
    }

    /// The change `hash` is, as this commit names it: the unit and the position
    /// in it, when the action made that change.
    #[must_use]
    pub fn change(&self, hash: &[u8; 32]) -> Option<&CommitPart> {
        self.parts.iter().find(|part| part.changes.contains(hash))
    }

    /// Frame the document payload `inner` under this commit: what one unit's
    /// signed envelope carries, so the device signature covers the commit as
    /// well as the change.
    ///
    /// # Errors
    /// [`CommitError::TooLarge`] for a commit over [`MAX_COMMIT_BYTES`].
    pub fn frame(&self, inner: &[u8]) -> Result<Vec<u8>, CommitError> {
        let commit = self.encode();
        let len = u32::try_from(commit.len()).map_err(|_| CommitError::TooLarge {
            actual: commit.len(),
            max: MAX_COMMIT_BYTES,
        })?;
        if commit.len() > MAX_COMMIT_BYTES {
            return Err(CommitError::TooLarge {
                actual: commit.len(),
                max: MAX_COMMIT_BYTES,
            });
        }
        let mut framed = Vec::with_capacity(
            COMMIT_FRAME_PREFIX
                .len()
                .saturating_add(4)
                .saturating_add(commit.len())
                .saturating_add(inner.len()),
        );
        framed.extend_from_slice(COMMIT_FRAME_PREFIX);
        framed.extend_from_slice(&len.to_le_bytes());
        framed.extend_from_slice(&commit);
        framed.extend_from_slice(inner);
        Ok(framed)
    }

    /// The commit the signed envelope whose payload is `cleartext` carries,
    /// when it carries one, and the document frame inside it.
    ///
    /// The commit travels inside the envelope's signed payload, so the
    /// device signature over that payload is the commit's signature. A commit
    /// naming another device than the envelope's author is refused.
    ///
    /// # Errors
    /// [`CommitError`] for bytes that are not a framed payload, a commit that
    /// does not check, or a commit authored by another device.
    pub fn of_envelope(envelope: &SignedOp) -> Result<Option<Self>, CommitError> {
        Self::of_envelope_payload(envelope, &envelope.payload)
    }

    /// The commit carried inside `cleartext`, the payload `envelope` signs once
    /// it is opened: a sealed route's plaintext, or an unsealed envelope's
    /// payload. `None` when the payload carries none.
    ///
    /// # Errors
    /// [`CommitError::ForeignDevice`] when the commit names another device than
    /// the envelope's author, or the [`CommitError`] of a commit that does not
    /// decode.
    pub fn of_envelope_payload(
        envelope: &SignedOp,
        cleartext: &[u8],
    ) -> Result<Option<Self>, CommitError> {
        let (_, commit) = Self::split(cleartext)?;
        let Some(commit) = commit else {
            return Ok(None);
        };
        if envelope.author.as_bytes() != &commit.author.device {
            return Err(CommitError::ForeignDevice);
        }
        Ok(Some(commit))
    }

    /// Split one signed document payload into the commit it carries (if any)
    /// and the document frame inside it. Payloads without a commit are
    /// returned whole, so every earlier frame keeps its meaning.
    ///
    /// # Errors
    /// [`CommitError`] for a frame whose commit length runs past its bytes,
    /// a commit over [`MAX_COMMIT_BYTES`], or a commit that does not check.
    pub fn split(cleartext: &[u8]) -> Result<(&[u8], Option<Self>), CommitError> {
        let Some(rest) = cleartext.strip_prefix(COMMIT_FRAME_PREFIX) else {
            return Ok((cleartext, None));
        };
        let (len, rest) = rest
            .split_first_chunk::<4>()
            .ok_or(CommitError::Truncated)?;
        let len = usize::try_from(u32::from_le_bytes(*len)).unwrap_or(usize::MAX);
        if len > MAX_COMMIT_BYTES {
            return Err(CommitError::TooLarge {
                actual: len,
                max: MAX_COMMIT_BYTES,
            });
        }
        let (body, inner) = rest.split_at_checked(len).ok_or(CommitError::Truncated)?;
        let commit = Self::decode(body)?;
        Ok((inner, Some(commit)))
    }
}

/// The head every version of a commit starts with: what it is and how to
/// read the rest.
#[derive(Deserialize)]
struct Head {
    #[allow(dead_code, reason = "read to reach the version beside it")]
    kind: String,
    version: u16,
}

/// A commit as version 1 wrote it: the action's name, item and invocation,
/// and the changes of each part.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct V1 {
    kind: String,
    #[allow(dead_code, reason = "the head chose this shape by reading it")]
    version: u16,
    workspace: String,
    author: CommitAuthor,
    at_ms: u64,
    action: CommitActionV1,
    parts: Vec<CommitPartV1>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CommitActionV1 {
    name: String,
    item: Option<String>,
    invocation: Option<uuid::Uuid>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CommitPartV1 {
    app: String,
    record: String,
    changes: Vec<[u8; 32]>,
}

/// One unit's changes, read for the commit that names them.
struct Read {
    /// The unit as the commit names it: which app's record, which changes.
    part: CommitPart,
    /// The one invocation every change of the unit ran under, when they all
    /// ran under the same one: the action they are all part of.
    signature: Option<uuid::Uuid>,
    /// The run the unit's changes were invoked in.
    run: Option<CommitGroup>,
    /// Who invoked the action.
    principal: Option<String>,
    /// When its latest change was made, in milliseconds since the Unix epoch.
    at_ms: u64,
}

impl Read {
    fn of(unit: &UnitMutation<'_>) -> Result<Option<Self>, CommitError> {
        let DocumentMutation::Incremental(bytes) = unit.mutation else {
            return Ok(None);
        };
        let changes =
            claim::parse_changes(bytes).map_err(|error| CommitError::Unnamed(error.to_string()))?;
        if changes.is_empty() {
            return Ok(None);
        }
        let mut read = Self {
            part: CommitPart {
                app: unit.app.to_owned(),
                record: unit.record.to_owned(),
                changes: Vec::with_capacity(changes.len()),
            },
            signature: None,
            run: None,
            principal: None,
            at_ms: 0,
        };
        let mut one_action = true;
        let mut first = None;
        let mut entries = tilo_actions::EntryReader::default();
        for (position, change) in changes.iter().enumerate() {
            read.part.changes.push(change.hash().0);
            read.at_ms = read.at_ms.max(
                u64::try_from(change.timestamp())
                    .unwrap_or(0)
                    .saturating_mul(1000),
            );
            let context = entries
                .read(change.actor_id(), change.message())
                .or_else(|| {
                    unit.document
                        .and_then(|document| tilo_actions::entry_of(document, change.hash()))
                });
            let Some(context) = context else {
                // A change that names no action is no part of one, and a
                // commit is the one record of an action.
                return Err(CommitError::Unnamed(change.hash().to_string()));
            };
            // The first change that says who invoked it speaks for the
            // action: every change of one action shares that context.
            if read.principal.is_none() {
                read.principal = context
                    .invoker
                    .as_ref()
                    .map(|invoker| invoker.actor.clone());
            }
            if read.run.is_none() {
                read.run = context.group.as_ref().map(|group| CommitGroup {
                    id: group.id.clone(),
                    label: group.label.clone(),
                    kind: group.kind.clone(),
                });
            }
            let ran_under = context.source_invocation.map(|id| id.0);
            if position == 0 {
                first = ran_under;
            } else if first != ran_under {
                one_action = false;
            }
        }
        read.signature = one_action.then_some(first).flatten();
        Ok(Some(read))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DocumentFrame, DocumentMutation, field_edit_mutation};
    use tilo_actions::{ActionContext, Did, FieldEdit, InvocationId};

    fn author(device: [u8; 32]) -> CommitAuthor {
        CommitAuthor {
            principal: Some("alice".to_owned()),
            device,
        }
    }

    fn part(app: &str, record: &str, change: u8) -> CommitPart {
        CommitPart {
            app: app.to_owned(),
            record: record.to_owned(),
            changes: vec![[change; 32]],
        }
    }

    fn two_part() -> WorkspaceCommit {
        WorkspaceCommit::new(
            "workspace",
            author([7; 32]),
            1_700_000_000_000,
            Some(uuid::Uuid::from_u128(9)),
            Some(CommitGroup {
                id: "run-1".to_owned(),
                label: "Alex".to_owned(),
                kind: "agent-run".to_owned(),
            }),
            vec![part("tasks", "a", 1), part("tasks", "b", 2)],
        )
    }

    /// One signed envelope carrying `cleartext`, as the device `device` sent
    /// it.
    fn envelope(cleartext: Vec<u8>, device: [u8; 32]) -> SignedOp {
        SignedOp::new_unsigned(
            cleartext,
            crate::document_artifact_id("tasks", "workspace", "a"),
            crate::Ed25519PublicKey::from_bytes(device),
            crate::AttestationId::new(),
            crate::OpKind::Change,
            std::time::UNIX_EPOCH,
            1,
        )
    }

    #[test]
    fn a_commit_round_trips_and_names_its_parts() {
        let commit = two_part();
        let bytes = commit.encode();
        assert_eq!(WorkspaceCommit::decode(&bytes).as_ref(), Ok(&commit));
        assert_eq!(commit.id(), WorkspaceCommit::decode(&bytes).unwrap().id());
        assert!(commit.covers("tasks", "a"));
        assert!(commit.covers("tasks", "b"));
        assert!(!commit.covers("tasks", "c"));
        assert_eq!(commit.part("tasks", "b"), Some(&part("tasks", "b", 2)));
        assert_eq!(
            commit.change(&[2; 32]).map(|part| part.record.as_str()),
            Some("b")
        );
        assert_eq!(commit.change(&[3; 32]), None);
        assert_eq!(commit.author, author([7; 32]));
        assert_eq!(commit.invocation, Some(uuid::Uuid::from_u128(9)));
        assert_eq!(
            commit.run.as_ref().map(|run| run.kind.as_str()),
            Some("agent-run")
        );
    }

    /// Write one change of `field` = `value` into `doc`, as the action
    /// `field` invoked under `invocation` made it.
    fn action_change(
        doc: &mut automerge::AutoCommit,
        field: &str,
        value: &str,
        invocation: InvocationId,
    ) -> automerge::ChangeHash {
        use automerge::transaction::Transactable as _;
        doc.put(automerge::ROOT, field, value).unwrap();
        let context = ActionContext {
            what: Some(Did::Edit(FieldEdit {
                ty: "tasks".to_owned(),
                field: field.to_owned(),
                path: vec![tilo_actions::PathStep::Field(field.to_owned())],
                op: tilo_actions::EditOp::Set,
                old: None,
                new: None,
                args: std::collections::BTreeMap::new(),
            })),
            invoker: Some(tilo_actions::ActionInvoker::actor("alice")),
            source_invocation: Some(invocation),
            ..ActionContext::for_resource("tasks", "tasks", field)
        };
        let mut options = automerge::transaction::CommitOptions::default().with_time(1_700_000_000);
        options.set_message(context.change_message().unwrap());
        doc.commit_with(options).unwrap()
    }

    /// The mutation one action made in a unit holding `field` = `value`.
    fn action_mutation(field: &str, value: &str, invocation: InvocationId) -> DocumentMutation {
        let mut doc = automerge::AutoCommit::new();
        action_change(&mut doc, field, value, invocation);
        DocumentMutation::Incremental(doc.save_after(&[]))
    }

    #[test]
    fn the_units_of_one_action_share_one_commit() {
        let invocation = InvocationId::new();
        let other = InvocationId::new();
        let title = action_mutation("title", "a", invocation);
        let body = action_mutation("body", "b", invocation);
        let elsewhere = action_mutation("title", "c", other);
        let mut both = automerge::AutoCommit::new();
        action_change(&mut both, "title", "d", invocation);
        action_change(&mut both, "body", "e", other);
        let mixed = DocumentMutation::Incremental(both.save_after(&[]));
        let snapshot = DocumentMutation::Snapshot(Vec::new());
        let units = [
            UnitMutation {
                app: "tasks",
                record: "a",
                mutation: &title,
                document: None,
            },
            UnitMutation {
                app: "tasks",
                record: "b",
                mutation: &body,
                document: None,
            },
            UnitMutation {
                app: "chat",
                record: "t",
                mutation: &elsewhere,
                document: None,
            },
            UnitMutation {
                app: "tasks",
                record: "mixed",
                mutation: &mixed,
                document: None,
            },
            UnitMutation {
                app: "tasks",
                record: "s",
                mutation: &snapshot,
                document: None,
            },
        ];
        let commits = WorkspaceCommit::of_units("workspace", &author([0; 32]), &units).unwrap();
        assert_eq!(commits.len(), units.len(), "one entry per unit");

        let action = commits[0]
            .as_ref()
            .expect("a unit that changed travels under a commit");
        assert_eq!(
            commits[1].as_ref(),
            Some(action),
            "both units the action wrote travel under its one commit"
        );
        assert_eq!(action.parts.len(), 2);
        assert!(action.covers("tasks", "a") && action.covers("tasks", "b"));
        assert_eq!(action.parts[0].changes, vec![change_of(&title)]);
        assert_eq!(action.invocation, Some(invocation.0));
        assert_eq!(action.author.principal.as_deref(), Some("alice"));
        assert_eq!(
            action.author.device, [0; 32],
            "the transport stamps the device"
        );
        assert_eq!(
            action.at_ms, 1_700_000_000_000,
            "the action's own change time"
        );
        assert_eq!(
            WorkspaceCommit::decode(&action.encode()).as_ref(),
            Ok(action),
            "a commit of an action's units is a valid commit"
        );

        let single = commits[2].as_ref().expect("another action, another commit");
        assert_ne!(single.id(), action.id());
        assert_eq!(single.parts.len(), 1);
        let mixed = commits[3]
            .as_ref()
            .expect("a unit of several actions keeps a commit of its own");
        assert_eq!(mixed.parts.len(), 1, "it joins neither of its actions");
        assert_eq!(mixed.invocation, None, "several invocations name none");
        assert_ne!(mixed.id(), action.id());
        assert_eq!(commits[4], None, "a unit that carries no change has none");
    }

    /// The one change `mutation` carries.
    fn change_of(mutation: &DocumentMutation) -> [u8; 32] {
        let DocumentMutation::Incremental(bytes) = mutation else {
            return [0; 32];
        };
        claim::parse_changes(bytes)
            .ok()
            .and_then(|changes| changes.first().map(|change| change.hash().0))
            .unwrap_or_default()
    }

    #[test]
    fn a_commit_is_the_same_address_on_every_peer() {
        let once = two_part().id();
        let twice = two_part().id();
        assert_eq!(once, twice);
        let mut other = two_part();
        other.at_ms = 1;
        assert_ne!(once, other.id());
    }

    #[test]
    fn a_commit_that_names_nothing_or_twice_is_refused() {
        let mut wrong_kind = two_part();
        wrong_kind.kind = "tilo.chat".to_owned();
        assert_eq!(
            WorkspaceCommit::decode(&wrong_kind.encode()),
            Err(CommitError::Kind("tilo.chat".to_owned()))
        );

        let mut wrong_version = two_part();
        wrong_version.version = 9;
        assert_eq!(
            WorkspaceCommit::decode(&wrong_version.encode()),
            Err(CommitError::Version(9))
        );

        let mut none = two_part();
        none.parts.clear();
        assert_eq!(
            WorkspaceCommit::decode(&none.encode()),
            Err(CommitError::NoParts)
        );

        let mut twice = two_part();
        twice.parts[1] = part("tasks", "a", 3);
        assert_eq!(
            WorkspaceCommit::decode(&twice.encode()),
            Err(CommitError::DuplicatePart {
                app: "tasks".to_owned(),
                record: "a".to_owned()
            })
        );

        let mut empty = two_part();
        empty.parts[1].changes.clear();
        assert_eq!(
            WorkspaceCommit::decode(&empty.encode()),
            Err(CommitError::EmptyPart {
                app: "tasks".to_owned(),
                record: "b".to_owned()
            })
        );

        assert!(WorkspaceCommit::decode(b"not a commit").is_err());
    }

    #[test]
    fn a_document_frame_carries_its_commit_and_old_frames_still_decode() {
        let (mutation, _) = field_edit_mutation("tasks", "title", "write").unwrap();
        let frame = mutation.encode_frames().unwrap().remove(0);
        assert!(matches!(
            DocumentFrame::decode(&frame),
            Ok(DocumentFrame::Whole(DocumentMutation::Incremental(_)))
        ));
        assert_eq!(WorkspaceCommit::split(&frame).unwrap().1, None);

        let commit = two_part();
        let framed = commit.frame(&frame).unwrap();
        assert_eq!(
            DocumentFrame::decode(&framed),
            DocumentFrame::decode(&frame),
            "a framed document frame decodes as the frame inside it"
        );
        let (inner, carried) = WorkspaceCommit::split(&framed).unwrap();
        assert_eq!(inner, frame);
        assert_eq!(carried, Some(commit));
    }

    #[test]
    fn a_commit_is_read_off_the_envelope_that_signed_it() {
        let commit = two_part();
        let framed = commit.frame(b"frame").unwrap();
        assert_eq!(
            WorkspaceCommit::of_envelope(&envelope(framed.clone(), [7; 32]))
                .unwrap()
                .as_ref(),
            Some(&commit)
        );
        assert_eq!(
            WorkspaceCommit::of_envelope(&envelope(framed, [8; 32])),
            Err(CommitError::ForeignDevice),
            "another device's signature signs nothing of this action"
        );
        assert_eq!(
            WorkspaceCommit::of_envelope(&envelope(b"frame".to_vec(), [7; 32])).unwrap(),
            None
        );
        assert_eq!(
            WorkspaceCommit::split(b"tilo-commit-frame-v1\0\x20"),
            Err(CommitError::Truncated)
        );
        let mut over = COMMIT_FRAME_PREFIX.to_vec();
        over.extend_from_slice(&u32::try_from(MAX_COMMIT_BYTES + 1).unwrap().to_le_bytes());
        assert!(matches!(
            WorkspaceCommit::split(&over),
            Err(CommitError::TooLarge { .. })
        ));
        assert_eq!(
            WorkspaceCommit::split(b"tilo-commit-frame-v1\0"),
            Err(CommitError::Truncated)
        );
    }

    #[test]
    fn a_sealed_part_carries_its_commit_in_the_plaintext() {
        // A sealed route's envelope holds the ciphertext of the payload; the
        // commit is inside what only the unit's readers open.
        let commit = two_part();
        let cleartext = commit.frame(b"frame").unwrap();
        let sealed = envelope(b"ciphertext".to_vec(), [7; 32]);
        assert_eq!(
            WorkspaceCommit::of_envelope(&sealed).unwrap(),
            None,
            "the sealed payload says nothing to a reader without the keys"
        );
        assert_eq!(
            WorkspaceCommit::of_envelope_payload(&sealed, &cleartext)
                .unwrap()
                .as_ref(),
            Some(&commit)
        );
        assert_eq!(
            WorkspaceCommit::of_envelope_payload(&envelope(cleartext.clone(), [8; 32]), &cleartext),
            Err(CommitError::ForeignDevice)
        );
    }

    #[test]
    fn an_action_publishing_changes_makes_one_commit_naming_them() {
        use automerge::transaction::Transactable as _;

        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "title", "child").unwrap();
        let invocation = InvocationId(uuid::Uuid::from_u128(42));
        let context = ActionContext {
            what: Some(Did::Edit(FieldEdit {
                ty: "tasks".to_owned(),
                field: "title".to_owned(),
                path: vec![tilo_actions::PathStep::Field("title".to_owned())],
                op: tilo_actions::EditOp::Set,
                old: None,
                new: None,
                args: std::collections::BTreeMap::new(),
            })),
            invoker: Some(tilo_actions::ActionInvoker::actor("alice")),
            source_invocation: Some(invocation),
            ..ActionContext::for_resource("tasks", "tasks", "a")
        };
        let mut options = automerge::transaction::CommitOptions::default();
        options.set_message(context.change_message().unwrap());
        // The executor stamps a change with the envelope's time (seconds).
        options.set_time(1_700_000_000);
        let hash = doc.commit_with(options).unwrap();

        let mutation = DocumentMutation::Incremental(doc.save_after(&[]));
        let commit = WorkspaceCommit::of_mutation(
            "workspace",
            "tasks",
            "a",
            &CommitAuthor {
                principal: None,
                device: [7; 32],
            },
            &mutation,
        )
        .unwrap()
        .unwrap();
        assert_eq!(commit.invocation, Some(invocation.0));
        assert_eq!(commit.author.principal.as_deref(), Some("alice"));
        assert_eq!(commit.parts[0].changes, vec![hash.0]);
        assert_eq!(
            commit.at_ms, 1_700_000_000_000,
            "the action keeps the time its changes carry"
        );

        assert_eq!(
            WorkspaceCommit::of_mutation(
                "workspace",
                "tasks",
                "a",
                &author([7; 32]),
                &DocumentMutation::Snapshot(vec![1, 2, 3]),
            )
            .unwrap(),
            None,
            "a snapshot carries no change to commit"
        );
    }

    #[test]
    fn a_change_that_names_no_action_commits_nothing() {
        use automerge::transaction::Transactable as _;
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "title", "child").unwrap();
        doc.commit();
        let mutation = DocumentMutation::Incremental(doc.save_after(&[]));
        assert!(matches!(
            WorkspaceCommit::of_mutation("workspace", "tasks", "a", &author([7; 32]), &mutation),
            Err(CommitError::Unnamed(_))
        ));
    }

    /// The next letter of a typed word continues its first letter's change:
    /// it names its action through that change, in the same mutation or, from
    /// the document, in one an earlier pass carried.
    #[test]
    fn a_continued_gesture_commits_to_its_action() {
        use automerge::transaction::Transactable as _;
        let gesture = tilo_actions::ActionId(uuid::Uuid::new_v4());
        let typed = |text: &str| ActionContext {
            action: Some(tilo_actions::EncodedAction {
                type_id: "notes.type".into(),
                payload: format!("{text:?}").into_bytes(),
            }),
            invoker: Some(tilo_actions::ActionInvoker::actor("alice")),
            ..ActionContext::default().in_gesture(Some(gesture))
        };
        let mut doc = automerge::AutoCommit::new();
        for (text, message) in [
            ("a", typed("a").change_message().unwrap()),
            ("ab", typed("ab").continuation_of(&typed("a")).unwrap()),
        ] {
            doc.put(automerge::ROOT, "body", text).unwrap();
            doc.commit_with(automerge::transaction::CommitOptions::default().with_message(message));
        }
        let first = doc.get_changes(&[])[0].hash();
        let unit = |mutation, document| UnitMutation {
            app: "notes",
            record: "",
            mutation,
            document,
        };
        let whole = DocumentMutation::Incremental(doc.save_after(&[]));
        let commit =
            WorkspaceCommit::of_units("workspace", &author([0; 32]), &[unit(&whole, None)])
                .unwrap()[0]
                .clone()
                .expect("both letters are one action");
        assert_eq!(commit.parts[0].changes.len(), 2);

        let next = DocumentMutation::Incremental(doc.save_after(&[first]));
        assert!(matches!(
            WorkspaceCommit::of_units("workspace", &author([0; 32]), &[unit(&next, None)]),
            Err(CommitError::Unnamed(_))
        ));
        let commit = WorkspaceCommit::of_units(
            "workspace",
            &CommitAuthor {
                principal: None,
                device: [0; 32],
            },
            &[unit(&next, Some(doc.document()))],
        )
        .unwrap()[0]
            .clone()
            .expect("the next letter alone is still its action's");
        assert_eq!(commit.author.principal.as_deref(), Some("alice"));
    }

    #[test]
    fn a_field_edit_is_a_commit_too() {
        let (mutation, claims) = field_edit_mutation("tasks", "title", "write").unwrap();
        let commit =
            WorkspaceCommit::of_mutation("workspace", "tasks", "", &author([3; 32]), &mutation)
                .unwrap()
                .unwrap();
        assert_eq!(commit.parts[0].changes, vec![claims[0].change]);
    }

    #[test]
    fn a_commit_is_a_record_kind_of_its_own() {
        assert_eq!(two_part().kind, COMMIT_RECORD_KIND);
        assert_eq!(two_part().version, COMMIT_VERSION);
    }

    #[test]
    fn a_version_1_commit_still_reads_with_nothing_it_never_carried() {
        let old = V1 {
            kind: COMMIT_RECORD_KIND.to_owned(),
            version: COMMIT_VERSION_NAMED_ACTION,
            workspace: "workspace".to_owned(),
            author: author([7; 32]),
            at_ms: 1_700_000_000_000,
            action: CommitActionV1 {
                name: "create".to_owned(),
                item: Some("child".to_owned()),
                invocation: Some(uuid::Uuid::from_u128(5)),
            },
            parts: vec![CommitPartV1 {
                app: "tasks".to_owned(),
                record: "a".to_owned(),
                changes: vec![[5; 32]],
            }],
        };
        let bytes = crate::canonical::canonical_bytes(&old).unwrap();
        let read = WorkspaceCommit::decode(&bytes).expect("a version 1 commit reads");
        assert_eq!(read.version, COMMIT_VERSION);
        assert_eq!(
            read.invocation,
            Some(uuid::Uuid::from_u128(5)),
            "the invocation it ran under carries over"
        );
        assert_eq!(read.run, None, "version 1 named no run");
        assert_eq!(read.parts[0].changes, vec![[5; 32]]);
        assert_eq!(
            read.encode(),
            WorkspaceCommit::new(
                "workspace",
                author([7; 32]),
                1_700_000_000_000,
                read.invocation,
                read.run.clone(),
                read.parts,
            )
            .encode(),
            "a version 1 commit writes as version 2 once read"
        );
    }

    #[test]
    fn an_action_names_the_run_it_ran_in_across_its_units() {
        use automerge::transaction::Transactable as _;
        let invocation = InvocationId::new();
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "title", "value").unwrap();
        let context = ActionContext {
            what: Some(Did::verb(
                tilo_actions::ActionKey::new("tasks", "note", "complete"),
                std::collections::BTreeMap::new(),
            )),
            group: Some(tilo_actions::WorkGroup {
                id: "run-1".to_owned(),
                label: "Alex".to_owned(),
                kind: "agent-run".to_owned(),
                title: None,
            }),
            invoker: Some(tilo_actions::ActionInvoker::actor("alice")),
            source_invocation: Some(invocation),
            ..ActionContext::for_resource("tasks", "tasks", "note:1")
        };
        let mut options = automerge::transaction::CommitOptions::default().with_time(1_700_000_000);
        options.set_message(context.change_message().unwrap());
        doc.commit_with(options).unwrap();
        let mutation = DocumentMutation::Incremental(doc.save_after(&[]));
        let commit =
            WorkspaceCommit::of_mutation("workspace", "tasks", "", &author([7; 32]), &mutation)
                .unwrap()
                .unwrap();

        let run = commit.run.as_ref().expect("the run it ran in");
        assert_eq!(run.id, "run-1");
        assert_eq!(run.kind, "agent-run");
        assert_eq!(run.label, "Alex");
        assert_eq!(
            WorkspaceCommit::decode(&commit.encode()).as_ref(),
            Ok(&commit),
            "the run travels with the commit"
        );
    }
}
