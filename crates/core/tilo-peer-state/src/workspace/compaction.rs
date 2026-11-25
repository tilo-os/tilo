//! A workspace document's signed compaction: the base it starts over from
//! ([`super::base`]), made and signed by a device of its owner or of one of its
//! administrators.
//!
//! A member admitted later must not receive what was withdrawn before, so the
//! workspace's history starts over from a base holding its state as it
//! stands. Whoever carries that base (a holder, a server that is not on the
//! member list, any member) carries it unchanged: the base names the device
//! that made it, and that device's signature over the base's change says the
//! base is the one it made. A receiver takes a newer base only with that
//! signature, and only from a device of a person the base's own signed member
//! list names as the workspace's owner or one of its administrators. A device
//! that holds nothing of the workspace yet takes its history only from the
//! workspace's founding or from such a compaction ([`verified_start`]), so a
//! carrier cannot hand a newcomer a history of its own making.
//!
//! The signature sits in a change of its own on the base: a change cannot sign
//! its own hash.

use automerge::transaction::Transactable as _;
use automerge::{ActorId, AutoCommit, ROOT, ReadDoc as _, ScalarValue, Value};
use collab_types::signer::DeviceSigner;
use tilo_user_identity::DeviceCert;

use crate::members::{MembershipOpCommand, SignedMembershipOp};

/// The root key holding the base's compaction, a [`SignedMembershipOp`]
/// whose command is [`MembershipOpCommand::Compact`], written in a change on
/// the base change.
pub const COMPACTION_KEY: &str = "base_compaction_v1";

/// Why a history's start is not one a device takes in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactionRefusal {
    /// The document starts from a base that names no device that made it.
    Unsigned,
    /// The base's signature does not hold: it is not of this base, this
    /// workspace or this generation, or not its named device's.
    Unproven(String),
    /// The device that made the base is of a person the base's member list
    /// does not name as the workspace's owner or one of its administrators.
    NotAdministrator,
    /// The document starts neither at the workspace's founding by its owner
    /// nor at a base.
    NotFounded,
}

impl std::fmt::Display for CompactionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsigned => f.write_str("the workspace's base names no device that made it"),
            Self::Unproven(reason) => {
                write!(f, "the workspace's base does not prove its maker: {reason}")
            }
            Self::NotAdministrator => f.write_str(
                "the workspace's base was made by a device of someone who is neither its owner \
                 nor one of its administrators",
            ),
            Self::NotFounded => f.write_str(
                "the workspace's history starts neither at its founding by its owner nor at a \
                 base its owner or an administrator signed",
            ),
        }
    }
}

impl std::error::Error for CompactionRefusal {}

/// A document holding `doc`'s state as it stands, at the next base
/// generation ([`super::base::rebase`]), signed by `device` with `key` at
/// `at_ns` as a compaction of `workspace`. Who may make one is the receivers'
/// to decide ([`compacted_by`]).
///
/// # Errors
///
/// The base could not be built or signed.
pub fn compact(
    doc: &AutoCommit,
    actor: ActorId,
    (device, key): (&DeviceCert, &dyn DeviceSigner),
    workspace: &str,
    at_ns: u64,
) -> Result<AutoCommit, String> {
    let mut based = super::base::rebase(doc, actor).map_err(|error| error.to_string())?;
    let base = super::base::root(&mut based).ok_or("a base has a change of its own")?;
    let command = MembershipOpCommand::Compact {
        workspace: workspace.to_owned(),
        generation: super::base::generation(&based),
        base: base.0,
    };
    let signed = SignedMembershipOp::signed(device, key, Vec::new(), at_ns, command)?;
    based
        .put(ROOT, COMPACTION_KEY, ScalarValue::Bytes(signed.to_bytes()?))
        .map_err(|error| error.to_string())?;
    based.commit_with(automerge::transaction::CommitOptions::default().with_time(0));
    Ok(based)
}

/// Whether `doc` holds nothing past the base it starts from: its base change,
/// and the change signing it if it is signed.
pub fn nothing_past_base(doc: &mut AutoCommit) -> bool {
    let Some(base) = super::base::root(doc) else {
        return false;
    };
    let past = doc.get_changes(&[base]).len();
    past == 0 || (past == 1 && named(doc).is_some())
}

/// The compaction `doc` starts from, as its base names it, unverified.
fn named(doc: &AutoCommit) -> Option<SignedMembershipOp> {
    match doc.get(ROOT, COMPACTION_KEY) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => tilo_failure::ignored!(
                "a signature that does not decode names no maker: the base reads as unsigned",
                postcard::from_bytes(bytes).ok()
            ),
            _ => None,
        },
        _ => None,
    }
}

/// The person whose device made and signed the base `doc` of `workspace`
/// starts from, when the base's own member list names that person its owner
/// or one of its administrators, and descends from `owner`, the owner the
/// receiver knows. The member list is read at the base: what changes came
/// after it hold is not the base's.
///
/// # Errors
///
/// `doc` starts from no base, from one that names no maker or one whose
/// signature does not hold, or from one made by anyone else.
pub fn compacted_by(
    doc: &AutoCommit,
    workspace: &str,
    owner: &[u8; 32],
) -> Result<[u8; 32], CompactionRefusal> {
    compacted_under(doc, workspace, Some(owner))
}

/// [`compacted_by`], for a receiver that knows the workspace's owner as
/// `owner`, or knows no owner yet (`None`) and takes the one the base's own
/// member list names.
fn compacted_under(
    doc: &AutoCommit,
    workspace: &str,
    owner: Option<&[u8; 32]>,
) -> Result<[u8; 32], CompactionRefusal> {
    let mut doc = doc.clone();
    let base = super::base::root(&mut doc).ok_or(CompactionRefusal::NotFounded)?;
    let signed = named(&doc).ok_or(CompactionRefusal::Unsigned)?;
    let MembershipOpCommand::Compact {
        workspace: named,
        generation,
        base: signed_base,
    } = &signed.command
    else {
        return Err(CompactionRefusal::Unsigned);
    };
    if *signed_base != base.0 || named != workspace || *generation != super::base::generation(&doc)
    {
        return Err(CompactionRefusal::Unproven(
            "the signature is of another base".into(),
        ));
    }
    signed.verify().map_err(CompactionRefusal::Unproven)?;
    let at_base = doc
        .fork_at(&[base])
        .map_err(|error| CompactionRefusal::Unproven(error.to_string()))?;
    let person = signed.author.user_public_key;
    if !administers(&at_base, workspace, owner, &person) {
        return Err(CompactionRefusal::NotAdministrator);
    }
    Ok(person)
}

/// Whether `doc`'s member list names `person` as the workspace's owner or one
/// of its administrators, and descends from `owner`, the owner the receiver
/// knows, if it knows one: the list replays from `owner`, or moved to its
/// owner from them by the transfers it carries, which only an owner's device
/// signs.
fn administers(
    doc: &AutoCommit,
    workspace: &str,
    owner: Option<&[u8; 32]>,
    person: &[u8; 32],
) -> bool {
    let policy = crate::members::replay(doc, workspace);
    if !known_owner(doc, &policy, owner) {
        return false;
    }
    let person = hex::encode(person);
    policy.owner.as_deref() == Some(person.as_str()) || policy.is_administrator(&person)
}

/// Whether `doc`'s member list, `policy` as it replays, is of the workspace
/// whose owner the receiver knows as `owner`: it names them its owner, or
/// replays from them as the owner its `owner` row names. A receiver that
/// knows no owner yet takes the one the list names.
fn known_owner(
    doc: &AutoCommit,
    policy: &tilo_action_policy::WorkspacePolicy,
    owner: Option<&[u8; 32]>,
) -> bool {
    let Some(owner) = owner else {
        return true;
    };
    let owner = hex::encode(owner);
    policy.owner.as_deref() == Some(owner.as_str())
        || super::governance_of(Some(doc)).owner.as_deref() == Some(owner.as_str())
}

/// That `doc`, the whole history a device that holds nothing of `workspace`
/// yet takes in, starts at the workspace's founding or at a base its owner or
/// an administrator signed ([`compacted_by`]). Such a device knows no owner
/// yet: it ties the history to the workspace by the devices it lists.
///
/// A document that never had a base starts at its founding: its `owner` row
/// names its founder, as its first change wrote it or as the founder's own
/// device signed at the root of its member list; and every operation of that
/// list written on no other is a device's of the founder, or one recording a
/// redemption, whose authority is the invitation replay checks and not its
/// writer.
///
/// # Errors
///
/// Why the history does not start where a newcomer takes it from.
pub fn verified_start(doc: &AutoCommit, workspace: &str) -> Result<(), CompactionRefusal> {
    if super::base::generation(doc) > 0 {
        return compacted_under(doc, workspace, None).map(drop);
    }
    let Some(founder) = super::governance_of(Some(doc)).owner else {
        return Err(CompactionRefusal::NotFounded);
    };
    let ops = crate::members::ops(doc);
    let roots: Vec<_> = ops.values().filter(|op| op.parents.is_empty()).collect();
    let signed_by_founder = roots.iter().any(|op| op.person() == founder);
    let admits = |op: &crate::members::SignedMembershipOp| {
        matches!(
            op.command,
            crate::members::MembershipOpCommand::Admit { .. }
        )
    };
    if !roots.iter().all(|op| op.person() == founder || admits(op)) {
        return Err(CompactionRefusal::NotFounded);
    }
    let starts_at_founding = signed_by_founder || {
        let mut whole = doc.clone();
        let first: Vec<_> = whole
            .get_changes(&[])
            .into_iter()
            .filter(|change| change.deps().is_empty())
            .map(|change| change.hash())
            .collect();
        let at_founding = whole
            .fork_at(&first)
            .map_err(|error| CompactionRefusal::Unproven(error.to_string()))?;
        super::governance_of(Some(&at_founding)).owner.as_deref() == Some(founder.as_str())
    };
    if starts_at_founding {
        Ok(())
    } else {
        Err(CompactionRefusal::NotFounded)
    }
}
