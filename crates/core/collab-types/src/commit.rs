//! One action, one workspace commit.
//!
//! A workspace's state is split into storage units: one Automerge document per
//! app and document, a chat's parts, a host's records. An action can write more
//! than one of them (creating a child writes the child's document and the
//! parent's reference to it), and publishing each unit on its own lets a peer
//! see half an action and leaves history without a name for the whole of it.
//!
//! A [`WorkspaceCommit`] is that name. It lists every unit the action wrote —
//! the app and document, and the heads of the changes it made there — under one
//! author, one time, and the invocation and run it was made in. A part's
//! document frame carries the commit inside its signed payload
//! ([`WorkspaceCommit::frame`]), so the device signature over that payload
//! signs the commit, and a receiver reads it off the envelope that delivered
//! the part ([`WorkspaceCommit::of_envelope_payload`]).
//!
//! The commit groups; it does not describe. Every change it names carries the
//! action that made it in its own history entry ([`tilo_actions::ActionContext`]):
//! that entry is what a grant is checked against, what replay verifies, and
//! what undo re-runs. The commit duplicates none of it — where the changes are,
//! so are the action's name, its resources, and the fields it wrote.
//!
//! The commit is a record of its own (version [`COMMIT_VERSION`]), so a host can store it beside the units it names. The
//! units, their channels and their seals keep their names: only the frame
//! around a document frame is new, and a frame without one is read exactly as
//! before.

use serde::{Deserialize, Serialize};

use crate::SignedOp;
use crate::op::DocumentMutation;

/// Version of the commit object; a receiver refuses one it does not know.
pub const COMMIT_VERSION: u16 = 4;
/// Prefix of a document frame that carries an action's commit: the commit's
/// byte length, the commit, then the document frame it belongs to.
pub const COMMIT_FRAME_PREFIX: &[u8] = b"tilo-commit\0";
/// Largest commit a receiver accepts, which bounds the bytes one framed
/// payload adds to a document frame beyond its mutation budget.
pub const MAX_COMMIT_BYTES: usize = 256 * 1024;

/// Who made an action: who invoked it, and the
/// device that signed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitAuthor {
    /// Who invoked the action, when it came through an action API that admitted them. `None` for
    /// a change nothing but its device accounts for.
    pub actor: Option<tilo_member_types::Actor>,
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
    /// Its label, as history names it, when it has one.
    pub label: Option<String>,
    /// Its kind (`agent-run`, `task-run`, `command-run`, …).
    pub kind: String,
}

/// One unit an action wrote: which app's document, and the heads of the
/// changes it made there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitPart {
    /// The app whose document the unit belongs to.
    pub app: String,
    /// The document the unit belongs to; empty for the app's route without a
    /// document (one document per app and workspace).
    pub document: String,
    /// The hash of each change the action made in this unit that no other
    /// of them depends on: those changes and what they depend on hold every
    /// change of the action, which the envelope carrying them names one by
    /// one in its claims.
    pub heads: Vec<[u8; 32]>,
}

impl CommitPart {
    /// Whether this part is the unit of `app`'s `document`.
    #[must_use]
    pub fn is(&self, app: &str, document: &str) -> bool {
        self.app == app && self.document == document
    }
}

/// One unit and what changed in it, as the publisher of a pass holds it.
#[derive(Clone, Copy)]
pub struct UnitMutation<'a> {
    /// The app whose document the unit belongs to.
    pub app: &'a str,
    /// The document the unit belongs to.
    pub document_id: &'a str,
    /// What changed in the unit during the pass.
    pub mutation: &'a DocumentMutation,
    /// The unit's document, holding the mutation's changes, when the
    /// publisher has it. A typed letter continues its writer's last whole
    /// entry and names its action only through it, which an earlier pass
    /// may have carried.
    pub document: Option<&'a automerge::Automerge>,
}

/// One signed action across every unit it wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCommit {
    /// [`COMMIT_VERSION`] — how to read it.
    pub version: u16,
    /// The workspace whose units the action wrote.
    pub workspace: uuid::Uuid,
    /// Who made it: who invoked it and the device that signed it.
    pub author: CommitAuthor,
    /// When the action was made, in milliseconds since the Unix epoch, as the
    /// author's device read the clock.
    pub at_ms: u64,
    /// The invocation the action ran under, when an admitted action API ran it.
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
    /// The bytes are not a commit.
    #[error("workspace commit does not decode: {0}")]
    Decode(String),
    /// The version is not [`COMMIT_VERSION`].
    #[error("workspace commit version {0} is not supported")]
    Version(u16),
    /// The commit names no unit, so it commits to nothing.
    #[error("workspace commit names no part")]
    NoParts,
    /// Two parts name the same unit.
    #[error("workspace commit names {app:?}/{document:?} twice")]
    DuplicatePart { app: String, document: String },
    /// A part names no change.
    #[error("workspace commit part {app:?}/{document:?} names no change")]
    EmptyPart { app: String, document: String },
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
        workspace: uuid::Uuid,
        author: CommitAuthor,
        at_ms: u64,
        invocation: Option<uuid::Uuid>,
        run: Option<CommitGroup>,
        parts: Vec<CommitPart>,
    ) -> Self {
        Self {
            version: COMMIT_VERSION,
            workspace,
            author,
            at_ms,
            invocation,
            run,
            parts,
        }
    }

    /// The commit for one action that published `mutation` as the unit of
    /// `app`'s `document`: the hash of every change it carries, the time the
    /// latest of them was made, and the invocation and run they agree on.
    /// `None` for a mutation that carries no change (a snapshot, or an empty
    /// set).
    ///
    /// # Errors
    /// [`CommitError::Unnamed`] for a change that names no action.
    pub fn of_mutation(
        workspace: uuid::Uuid,
        app: &str,
        document: &str,
        author: &CommitAuthor,
        mutation: &DocumentMutation,
    ) -> Result<Option<Self>, CommitError> {
        let unit = UnitMutation {
            app,
            document_id: document,
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
        workspace: uuid::Uuid,
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
        workspace: uuid::Uuid,
        author: &CommitAuthor,
        invocation: Option<uuid::Uuid>,
        first: &Read,
        parts: &[&Read],
    ) -> Self {
        let actor = parts
            .iter()
            .find_map(|part| part.actor.clone())
            .or_else(|| author.actor.clone());
        // The author's own change times, so every peer reads one time for the
        // action and a restarted one reads the same.
        let at_ms = parts.iter().map(|part| part.at_ms).max().unwrap_or(0);
        Self::new(
            workspace,
            CommitAuthor {
                actor,
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

    /// The commit of `bytes`, checked: a known version, at least one
    /// part, no unit twice, and a change in every part.
    ///
    /// # Errors
    /// [`CommitError`] for bytes that are not such a commit.
    pub fn decode(bytes: &[u8]) -> Result<Self, CommitError> {
        let commit = crate::canonical::from_canonical_bytes::<Self>(bytes)
            .map_err(|error| CommitError::Decode(error.to_string()))?;
        commit.check()?;
        Ok(commit)
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
    /// [`CommitError::Version`],
    /// [`NoParts`](CommitError::NoParts),
    /// [`DuplicatePart`](CommitError::DuplicatePart) or
    /// [`EmptyPart`](CommitError::EmptyPart).
    pub fn check(&self) -> Result<(), CommitError> {
        if self.version != COMMIT_VERSION {
            return Err(CommitError::Version(self.version));
        }
        if self.parts.is_empty() {
            return Err(CommitError::NoParts);
        }
        for (index, part) in self.parts.iter().enumerate() {
            if part.heads.is_empty() {
                return Err(CommitError::EmptyPart {
                    app: part.app.clone(),
                    document: part.document.clone(),
                });
            }
            if self
                .parts
                .iter()
                .take(index)
                .any(|earlier| earlier.is(&part.app, &part.document))
            {
                return Err(CommitError::DuplicatePart {
                    app: part.app.clone(),
                    document: part.document.clone(),
                });
            }
        }
        Ok(())
    }

    /// The part of `app`'s `document`, when the action wrote it.
    #[must_use]
    pub fn part(&self, app: &str, document: &str) -> Option<&CommitPart> {
        self.parts.iter().find(|part| part.is(app, document))
    }

    /// Whether the action wrote the unit of `app`'s `document`.
    #[must_use]
    pub fn covers(&self, app: &str, document: &str) -> bool {
        self.part(app, document).is_some()
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

/// One unit's changes, read for the commit that names them.
struct Read {
    /// The unit as the commit names it: which app's document, which heads.
    part: CommitPart,
    /// The one invocation every change of the unit ran under, when they all
    /// ran under the same one: the action they are all part of.
    signature: Option<uuid::Uuid>,
    /// The run the unit's changes were invoked in.
    run: Option<CommitGroup>,
    /// Who invoked the action.
    actor: Option<tilo_member_types::Actor>,
    /// When its latest change was made, in milliseconds since the Unix epoch.
    at_ms: u64,
}

impl Read {
    fn of(unit: &UnitMutation<'_>) -> Result<Option<Self>, CommitError> {
        let DocumentMutation::Incremental(bytes) = unit.mutation else {
            return Ok(None);
        };
        // A publisher reads what it publishes itself: the changes are its own.
        let changes = automerge::Automerge::parse_changes(bytes)
            .map_err(|error| CommitError::Unnamed(error.to_string()))?;
        if changes.is_empty() {
            return Ok(None);
        }
        let mut read = Self {
            part: CommitPart {
                app: unit.app.to_owned(),
                document: unit.document_id.to_owned(),
                heads: Vec::new(),
            },
            signature: None,
            run: None,
            actor: None,
            at_ms: 0,
        };
        let depended: std::collections::HashSet<_> = changes
            .iter()
            .flat_map(|change| change.deps().iter().copied())
            .collect();
        let mut one_action = true;
        let mut first = None;
        let mut entries = tilo_actions::EntryReader::default();
        for (position, change) in changes.iter().enumerate() {
            if !depended.contains(&change.hash()) {
                read.part.heads.push(change.hash().0);
            }
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
            if read.actor.is_none() {
                read.actor = context
                    .invoker
                    .as_ref()
                    .map(|invoker| invoker.acting.by.clone());
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

    const WORKSPACE: uuid::Uuid = uuid::Uuid::from_u128(1);

    fn author(device: [u8; 32]) -> CommitAuthor {
        CommitAuthor {
            actor: Some(tilo_member_types::MemberId::person("alice").into()),
            device,
        }
    }

    fn part(app: &str, document: &str, change: u8) -> CommitPart {
        CommitPart {
            app: app.to_owned(),
            document: document.to_owned(),
            heads: vec![[change; 32]],
        }
    }

    fn two_part() -> WorkspaceCommit {
        WorkspaceCommit::new(
            WORKSPACE,
            author([7; 32]),
            1_700_000_000_000,
            Some(uuid::Uuid::from_u128(9)),
            Some(CommitGroup {
                id: "run-1".to_owned(),
                // A group may be unnamed.
                label: None,
                kind: "agent-session".to_owned(),
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

    tilo_test_env::scenario! {
        /// The commit as a stored value: its encoding, its address, what it refuses and the version before this one.
        fn a_commit_is_a_durable_record_that_names_its_parts;

        phase a_commit_round_trips_and_names_its_parts;
        {
            let commit = two_part();
            let bytes = commit.encode();
            assert_eq!(WorkspaceCommit::decode(&bytes).as_ref(), Ok(&commit));
            assert_eq!(commit.id(), WorkspaceCommit::decode(&bytes).unwrap().id());
            assert!(commit.covers("tasks", "a"));
            assert!(commit.covers("tasks", "b"));
            assert!(!commit.covers("tasks", "c"));
            assert_eq!(commit.part("tasks", "b"), Some(&part("tasks", "b", 2)));
            assert_eq!(commit.author, author([7; 32]));
            assert_eq!(commit.invocation, Some(uuid::Uuid::from_u128(9)));
            assert_eq!(
                commit.run.as_ref().map(|run| run.kind.as_str()),
                Some("agent-session")
            );
        }

        phase a_commit_is_the_same_address_on_every_peer;
        {
            let once = two_part().id();
            let twice = two_part().id();
            assert_eq!(once, twice);
            let mut other = two_part();
            other.at_ms = 1;
            assert_ne!(once, other.id());
        }

        phase a_commit_that_names_nothing_or_twice_is_refused;
        {
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
                    document: "a".to_owned()
                })
            );

            let mut empty = two_part();
            empty.parts[1].heads.clear();
            assert_eq!(
                WorkspaceCommit::decode(&empty.encode()),
                Err(CommitError::EmptyPart {
                    app: "tasks".to_owned(),
                    document: "b".to_owned()
                })
            );

            assert!(WorkspaceCommit::decode(b"not a commit").is_err());
        }

        phase a_commit_is_a_record_of_its_own;
        {
            assert_eq!(two_part().version, COMMIT_VERSION);
            // A person's typed word: their key, one unit, one head.
            let word = WorkspaceCommit::new(
                uuid::Uuid::from_u128(0xbe40_ad30_af8b_4bb6_8ce1_61ae_844c_6e6c),
                CommitAuthor {
                    actor: Some(tilo_member_types::MemberId::person("d6".repeat(32)).into()),
                    device: [7; 32],
                },
                1_700_000_000_000,
                None,
                None,
                vec![part("so.tilo.notes", "", 1)],
            );
            assert!(word.encode().len() <= 200, "{}", word.encode().len());
        }
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
            invoker: Some(tilo_actions::ActionInvoker::person("alice")),
            source_invocation: Some(invocation),
            ..ActionContext::for_resource("tasks", "tasks", field)
        };
        let mut options = automerge::transaction::CommitOptions::default().with_time(1_700_000_000);
        options.set_message(context.entry_in(&*doc).unwrap());
        doc.commit_with(options).unwrap()
    }

    /// The mutation one action made in a unit holding `field` = `value`.
    fn action_mutation(field: &str, value: &str, invocation: InvocationId) -> DocumentMutation {
        let mut doc = automerge::AutoCommit::new();
        action_change(&mut doc, field, value, invocation);
        DocumentMutation::Incremental(doc.save_after(&[]))
    }

    tilo_test_env::scenario! {
        /// Which commit the changes of one action make, across the units it wrote.
        fn an_actions_changes_make_one_commit_across_its_units;

        phase the_units_of_one_action_share_one_commit;
        {
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
                    document_id: "a",
                    mutation: &title,
                    document: None,
                },
                UnitMutation {
                    app: "tasks",
                    document_id: "b",
                    mutation: &body,
                    document: None,
                },
                UnitMutation {
                    app: "chat",
                    document_id: "t",
                    mutation: &elsewhere,
                    document: None,
                },
                UnitMutation {
                    app: "tasks",
                    document_id: "mixed",
                    mutation: &mixed,
                    document: None,
                },
                UnitMutation {
                    app: "tasks",
                    document_id: "s",
                    mutation: &snapshot,
                    document: None,
                },
            ];
            let commits = WorkspaceCommit::of_units(WORKSPACE, &author([0; 32]), &units).unwrap();
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
            assert_eq!(action.parts[0].heads, vec![change_of(&title)]);
            assert_eq!(action.invocation, Some(invocation.0));
            assert_eq!(action.author.actor, Some(tilo_member_types::MemberId::person("alice").into()));
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

        phase an_action_publishing_changes_makes_one_commit_naming_them;
        {
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
                invoker: Some(tilo_actions::ActionInvoker::person("alice")),
                source_invocation: Some(invocation),
                ..ActionContext::for_resource("tasks", "tasks", "a")
            };
            let mut options = automerge::transaction::CommitOptions::default();
            options.set_message(context.entry_in(&doc).unwrap());
            // The executor stamps a change with the envelope's time (seconds).
            options.set_time(1_700_000_000);
            let hash = doc.commit_with(options).unwrap();

            let mutation = DocumentMutation::Incremental(doc.save_after(&[]));
            let commit = WorkspaceCommit::of_mutation(
                WORKSPACE,
                "tasks",
                "a",
                &CommitAuthor {
                    actor: None,
                    device: [7; 32],
                },
                &mutation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(commit.invocation, Some(invocation.0));
            assert_eq!(commit.author.actor, Some(tilo_member_types::MemberId::person("alice").into()));
            assert_eq!(commit.parts[0].heads, vec![hash.0]);
            assert_eq!(
                commit.at_ms, 1_700_000_000_000,
                "the action keeps the time its changes carry"
            );

            assert_eq!(
                WorkspaceCommit::of_mutation(
                    WORKSPACE,
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

        phase a_change_that_names_no_action_commits_nothing;
        {
            use automerge::transaction::Transactable as _;
            let mut doc = automerge::AutoCommit::new();
            doc.put(automerge::ROOT, "title", "child").unwrap();
            doc.commit();
            let mutation = DocumentMutation::Incremental(doc.save_after(&[]));
            assert!(matches!(
                WorkspaceCommit::of_mutation(WORKSPACE, "tasks", "a", &author([7; 32]), &mutation),
                Err(CommitError::Unnamed(_))
            ));
        }

        phase a_continued_gesture_commits_to_its_action;
        // The next letter of a typed word continues its first letter's change:
        // it names its action through that change, in the same mutation or, from
        // the document, in one an earlier pass carried.
        {
            use automerge::transaction::Transactable as _;
            let gesture = tilo_actions::ActionId(uuid::Uuid::new_v4());
            let typed = |text: &str| ActionContext {
                action: Some(tilo_actions::EncodedAction {
                    type_id: "notes.type".into(),
                    payload: format!("{text:?}").into_bytes(),
                }),
                invoker: Some(tilo_actions::ActionInvoker::person("alice")),
                ..ActionContext::default().in_gesture(Some(gesture))
            };
            let mut doc = automerge::AutoCommit::new();
            for (text, message) in [
                ("a", typed("a").entry_in(&doc).unwrap()),
                ("ab", typed("ab").continuation_of(&typed("a")).unwrap()),
            ] {
                doc.put(automerge::ROOT, "body", text).unwrap();
                doc.commit_with(automerge::transaction::CommitOptions::default().with_message(message));
            }
            let first = doc.get_changes(&[])[0].hash();
            let unit = |mutation, document| UnitMutation {
                app: "notes",
                document_id: "",
                mutation,
                document,
            };
            let whole = DocumentMutation::Incremental(doc.save_after(&[]));
            let commit =
                WorkspaceCommit::of_units(WORKSPACE, &author([0; 32]), &[unit(&whole, None)])
                    .unwrap()[0]
                    .clone()
                    .expect("both letters are one action");
            let last = doc.get_changes(&[])[1].hash();
            assert_eq!(commit.parts[0].heads, vec![last.0], "the second letter is built on the first");

            let next = DocumentMutation::Incremental(doc.save_after(&[first]));
            assert!(matches!(
                WorkspaceCommit::of_units(WORKSPACE, &author([0; 32]), &[unit(&next, None)]),
                Err(CommitError::Unnamed(_))
            ));
            let commit = WorkspaceCommit::of_units(
                WORKSPACE,
                &CommitAuthor {
                    actor: None,
                    device: [0; 32],
                },
                &[unit(&next, Some(doc.document()))],
            )
            .unwrap()[0]
                .clone()
                .expect("the next letter alone is still its action's");
            assert_eq!(commit.author.actor, Some(tilo_member_types::MemberId::person("alice").into()));
        }

        phase a_field_edit_is_a_commit_too;
        {
            let (mutation, claims) = field_edit_mutation("tasks", "title", "write", &[1; 32]).unwrap();
            let commit =
                WorkspaceCommit::of_mutation(WORKSPACE, "tasks", "", &author([3; 32]), &mutation)
                    .unwrap()
                    .unwrap();
            assert_eq!(commit.parts[0].heads, vec![claims[0].change]);
        }

        phase an_action_names_the_run_it_ran_in_across_its_units;
        {
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
                    label: Some("Rename the task".to_owned()),
                    kind: "agent-session".to_owned(),
                    declared_id: None,
                    session: None,
                }),
                invoker: Some(tilo_actions::ActionInvoker::person("alice")),
                source_invocation: Some(invocation),
                ..ActionContext::for_resource("tasks", "tasks", "note:1")
            };
            let mut options = automerge::transaction::CommitOptions::default().with_time(1_700_000_000);
            options.set_message(context.entry_in(&doc).unwrap());
            doc.commit_with(options).unwrap();
            let mutation = DocumentMutation::Incremental(doc.save_after(&[]));
            let commit =
                WorkspaceCommit::of_mutation(WORKSPACE, "tasks", "", &author([7; 32]), &mutation)
                    .unwrap()
                    .unwrap();

            let run = commit.run.as_ref().expect("the run it ran in");
            assert_eq!(run.id, "run-1");
            assert_eq!(run.kind, "agent-session");
            assert_eq!(run.label.as_deref(), Some("Rename the task"));
            assert_eq!(
                WorkspaceCommit::decode(&commit.encode()).as_ref(),
                Ok(&commit),
                "the run travels with the commit"
            );
        }
    }

    /// The one change `mutation` carries.
    fn change_of(mutation: &DocumentMutation) -> [u8; 32] {
        let DocumentMutation::Incremental(bytes) = mutation else {
            return [0; 32];
        };
        automerge::Automerge::parse_changes(bytes)
            .ok()
            .and_then(|changes| changes.first().map(|change| change.hash().0))
            .unwrap_or_default()
    }

    tilo_test_env::scenario! {
        /// Where a commit rides: the document frame, the signed envelope and the plaintext of a sealed part.
        fn a_commit_travels_with_the_parts_it_names;

        phase a_document_frame_carries_its_commit_and_old_frames_still_decode;
        {
            let (mutation, _) = field_edit_mutation("tasks", "title", "write", &[1; 32]).unwrap();
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

        phase a_commit_is_read_off_the_envelope_that_signed_it;
        {
            let commit = two_part();
            let framed = commit.frame(b"frame").unwrap();
            assert_eq!(
                WorkspaceCommit::of_envelope_payload(&envelope(vec![], [7; 32]), &framed)
                    .unwrap()
                    .as_ref(),
                Some(&commit)
            );
            assert_eq!(
                WorkspaceCommit::of_envelope_payload(&envelope(vec![], [8; 32]), &framed),
                Err(CommitError::ForeignDevice),
                "another device's signature signs nothing of this action"
            );
            assert_eq!(
                WorkspaceCommit::of_envelope_payload(&envelope(vec![], [7; 32]), b"frame").unwrap(),
                None
            );
            assert_eq!(
                WorkspaceCommit::split(b"tilo-commit\0\x20"),
                Err(CommitError::Truncated)
            );
            let mut over = COMMIT_FRAME_PREFIX.to_vec();
            over.extend_from_slice(&u32::try_from(MAX_COMMIT_BYTES + 1).unwrap().to_le_bytes());
            assert!(matches!(
                WorkspaceCommit::split(&over),
                Err(CommitError::TooLarge { .. })
            ));
            assert_eq!(
                WorkspaceCommit::split(b"tilo-commit\0"),
                Err(CommitError::Truncated)
            );
        }

        phase a_sealed_part_carries_its_commit_in_the_plaintext;
        {
            // A sealed route's envelope holds the ciphertext of the payload; the
            // commit is inside what only the unit's readers open.
            let commit = two_part();
            let cleartext = commit.frame(b"frame").unwrap();
            let sealed = envelope(b"ciphertext".to_vec(), [7; 32]);
            assert_eq!(
                WorkspaceCommit::of_envelope_payload(&sealed, &sealed.payload).unwrap(),
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
    }
}
