//! The one admission rule for a room document a peer sends (`SyncResp`).
//!
//! The native peer ([`crate::StatePeer`]) and the shell's workspace tree both
//! receive the same room document — owner, capabilities, identities, media
//! policy, coverage, durability, receipts and window contents — so both admit it
//! here. What differs between them is only data: how the receiver identifies
//! the sender ([`Writer`]) and whose room it is. Each caller keeps its own
//! storage and projection.
//!
//! The whole batch is refused or the whole candidate is returned: a partial
//! merge would advance local heads past refused operations.
use automerge::AutoCommit;
use tilo_action_policy::{ActionKind, Answer, Context, TreePath};

use super::media;
use tilo_desktop_model::projection::room_layout_choice;

/// How the receiver knows the sender's standing in the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The receiver resolved the sender to this person root, from a device
    /// held to the foreign-device bound or not. The batch needs undoable
    /// changes on the room allowed under the policy the receiver already
    /// holds.
    Member { person: [u8; 32], foreign: bool },
    /// The receiver's transport decided this sender may change the room before
    /// it delivered the batch, and the receiver has no roster entry of its own to
    /// decide it again (the shell's network gate, `peer_room_allows`). Every
    /// other check still applies.
    TransportAuthorized,
}

/// The sender of a batch, as the receiver's transport authenticated it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Writer<'a> {
    /// The authenticated sender key, spelt the way the topology registry
    /// names it.
    pub key: &'a str,
    pub standing: Standing,
}

impl Writer<'_> {
    fn person(&self) -> Option<&[u8; 32]> {
        match &self.standing {
            Standing::Member { person, .. } => Some(person),
            Standing::TransportAuthorized => None,
        }
    }

    /// Whether the sender is a device of `owner`: the room's governance is
    /// theirs to write, from any device their root certifies. The receiver
    /// knows the owner from what it joined through, never from the batch,
    /// so a first answer cannot make its sender the owner.
    fn governs(&self, owner: &[u8; 32]) -> bool {
        self.person() == Some(owner)
    }
}

/// Everything [`admit_room_changes`] decides a batch against.
#[derive(Clone, Copy, Debug)]
pub struct RoomAdmission<'a> {
    /// The room document the receiver holds now; the candidate is built on it.
    pub before: Option<&'a AutoCommit>,
    /// The room id as governance and capability scopes spell it.
    pub room: &'a str,
    pub source: Writer<'a>,
    /// The room's owner, whose devices write its governance.
    pub owner: &'a [u8; 32],
    /// The instant the batch is judged at, in nanoseconds since the Unix
    /// epoch: grant expiry, and the certificate a saved shortcut's proof
    /// carries. Not whole seconds: a certificate issued earlier in this
    /// second is already valid.
    pub now_ns: u64,
}

/// A batch every check admitted. The caller binds a returned media policy to
/// its own authority record before installing the candidate.
#[derive(Debug)]
pub struct AdmittedRoom {
    pub candidate: AutoCommit,
    pub media: Option<media::RoomMediaV1>,
    /// The candidate is a newer base ([`super::base`]) replacing the
    /// document held, not that document with the batch applied.
    pub rebased: bool,
}

/// Why a batch was refused. Display strings are the peer-state messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomRefusal {
    /// The sender is not the governance writer and nothing is held to check
    /// its membership against.
    GovernanceMissing,
    /// The sender may not change the room.
    WriteForbidden,
    /// The batch does not decode, or isolated admission refused it.
    Malformed(String),
    /// The document names a different room than the one it answers.
    DeclaredRoomMismatch,
    /// The batch changes governance and its sender is not the owner's device.
    GovernanceWriter,
    /// A member's membership change is not theirs to make.
    Membership(String),
    /// The batch rewrites an effect receipt.
    ReceiptImmutable,
    /// The batch records an effect receipt, or anchors archived ones,
    /// outside its writer's own slice ([`super::effect_slices_kept`]).
    ReceiptWriter,
    Coverage(String),
    /// The batch rewinds a durable receipt.
    Durability(String),
    /// The media policy is malformed or its transition is not permitted.
    Media(media::MediaPolicyError),
    /// The batch adds or changes the shortcut `0` naming who saved it
    /// without that person's device's proof ([`super::saved_shortcut`]).
    ShortcutAuthorship(String),
}

impl std::fmt::Display for RoomRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GovernanceMissing => f.write_str("room governance missing"),
            Self::WriteForbidden => f.write_str("room write forbidden"),
            Self::Malformed(error)
            | Self::Coverage(error)
            | Self::Durability(error)
            | Self::Membership(error) => f.write_str(error),
            Self::DeclaredRoomMismatch => f.write_str("declared room mismatch"),
            Self::GovernanceWriter => f.write_str("governance writer is not the owner's device"),
            Self::ReceiptImmutable => f.write_str("effect receipt is immutable"),
            Self::ReceiptWriter => f.write_str("effect receipt is not its writer's own"),
            Self::Media(error) => write!(f, "{error}"),
            Self::ShortcutAuthorship(shortcut) => write!(
                f,
                "the shortcut `{shortcut}` names who saved it without their device's proof"
            ),
        }
    }
}

impl std::error::Error for RoomRefusal {}

/// Admit `changes` to `input.before`: build the candidate and run every check a
/// room document must pass, in one place for every receiver.
///
/// 1. The sender may make undoable changes on the room, unless it is the
///    owner's device (who may grant itself anything) — [`Standing`].
/// 2. The batch decodes under the shared incoming limits ([`super::candidate`]).
/// 3. Then [`check_candidate`].
///
/// # Errors
///
/// The first [`RoomRefusal`] that applies; nothing is admitted then.
pub fn admit_room_changes(
    input: RoomAdmission<'_>,
    changes: &[Vec<u8>],
) -> Result<AdmittedRoom, RoomRefusal> {
    let governs = input.source.governs(input.owner);
    let writes = governs || may_write(input.before, input.room, &input.source, input.now_ns)?;
    match super::base::lineage(input.before, changes) {
        super::base::Lineage::Same { changes, replaced } => {
            // Changes waiting in the document that the batch shows to be of
            // another history go before the batch is applied.
            let pruned = super::base::pruned(input.before, replaced);
            let candidate = super::candidate(pruned.as_ref().or(input.before), &changes)
                .map_err(RoomRefusal::Malformed)?;
            // One who only reads the room records only their own receipts
            // and gives only their own shares.
            if !writes && !super::only_reader_writes_differ(input.before, &candidate) {
                return Err(RoomRefusal::WriteForbidden);
            }
            let membership = !governs
                && !super::governance_unchanged(input.before, &candidate)
                && membership_authorized(&input, &changes)?;
            let media = checked(&input, &candidate, membership)?;
            Ok(AdmittedRoom {
                candidate,
                media,
                rebased: false,
            })
        }
        super::base::Lineage::Newer(changes) if governs => {
            let candidate = super::candidate(None, &changes).map_err(RoomRefusal::Malformed)?;
            // The receiver projects what it owns onto the base it adopts, so a
            // base leaving out or changing its entries holds only until then.
            let media = checked(&input, &candidate, false)?;
            Ok(AdmittedRoom {
                candidate,
                media,
                rebased: true,
            })
        }
        super::base::Lineage::Newer(_) => Err(RoomRefusal::GovernanceWriter),
    }
}

/// Check a candidate room document against `input.before`, however it was
/// built: from `input.before` and a batch ([`admit_room_changes`]), or as a
/// whole checkpoint replacing it. Returns the candidate's media policy for the
/// caller to bind.
///
/// 1. The document names this room ([`super::declared_room_mismatches`]).
/// 2. A governance change — owner, capabilities, identities, media,
///    coverage, receipts, durability or import proposals — comes from the
///    owner's devices ([`super::governance_unchanged`]).
/// 3. The room's layout scope changes only by the owner's devices.
/// 4. Effect receipts are immutable ([`super::immutable_receipts`]), and
///    one not from the owner's devices records only its writer's own
///    ([`super::effect_slices_kept`]).
/// 5. Coverage and durable receipts never rewind
///    ([`crate::coverage::monotonic`], [`crate::durability::monotonic`]).
/// 6. The media policy is well formed and its transition is permitted
///    ([`media::validate_transition`]).
///
/// # Errors
///
/// The first [`RoomRefusal`] that applies.
pub fn check_candidate(
    input: &RoomAdmission<'_>,
    candidate: &AutoCommit,
) -> Result<Option<media::RoomMediaV1>, RoomRefusal> {
    checked(input, candidate, false)
}

/// A member's batch that changes governance, judged as a membership change
/// where its writer made it ([`super::membership`]). Returns whether it was
/// admitted as one.
fn membership_authorized(
    input: &RoomAdmission<'_>,
    changes: &[Vec<u8>],
) -> Result<bool, RoomRefusal> {
    let (Some(before), Standing::Member { person, foreign }) =
        (input.before, input.source.standing)
    else {
        return Err(RoomRefusal::GovernanceWriter);
    };
    let (at, after) = super::membership::position(before, changes).map_err(membership)?;
    super::membership::authorize(
        input.room,
        (&person, input.source.key, foreign),
        &at,
        changes,
        &after,
        input.now_ns,
    )
    .map_err(membership)?;
    Ok(true)
}

fn membership(refusal: super::membership::MembershipRefusal) -> RoomRefusal {
    match refusal {
        super::membership::MembershipRefusal::NotMembership => RoomRefusal::GovernanceWriter,
        refusal => RoomRefusal::Membership(refusal.to_string()),
    }
}

/// `membership`: the batch's governance change was admitted as its writer's
/// membership change, so only what membership cannot touch is checked here.
fn checked(
    input: &RoomAdmission<'_>,
    candidate: &AutoCommit,
    membership: bool,
) -> Result<Option<media::RoomMediaV1>, RoomRefusal> {
    let RoomAdmission {
        before,
        room,
        source,
        owner,
        now_ns,
    } = *input;
    let governs = source.governs(owner);
    let declared = uuid::Uuid::parse_str(room).map_err(|_| RoomRefusal::DeclaredRoomMismatch)?;
    if super::declared_room_mismatches(candidate, declared) {
        return Err(RoomRefusal::DeclaredRoomMismatch);
    }
    if !governs && !membership && !super::governance_unchanged(before, candidate) {
        return Err(RoomRefusal::GovernanceWriter);
    }
    if !governs {
        if before.and_then(room_layout_choice) != room_layout_choice(candidate) {
            return Err(RoomRefusal::GovernanceWriter);
        }
        if before.map_or_else(Vec::new, crate::room_hosts_of) != crate::room_hosts_of(candidate) {
            // The room's hosts are its owner's to name.
            return Err(RoomRefusal::GovernanceWriter);
        }
        // The owner's devices relay what members recorded; anyone else
        // records only their own.
        if !super::effect_slices_kept(before, candidate, source.person()) {
            return Err(RoomRefusal::ReceiptWriter);
        }
    }
    // Every writer, the owner's devices included: it relays and merges what members
    // write, so who sent a batch says nothing about who saved a shortcut.
    saved_shortcuts_proven(before, candidate, room, now_ns)?;
    if !super::immutable_receipts(before, candidate) {
        return Err(RoomRefusal::ReceiptImmutable);
    }
    crate::coverage::monotonic(before, candidate).map_err(RoomRefusal::Coverage)?;
    crate::durability::monotonic(before, candidate).map_err(RoomRefusal::Durability)?;
    media::validate_transition(before, candidate, media::TransitionMode::Admission)
        .map_err(RoomRefusal::Media)
}

/// A shortcut a batch adds or changes names, as who saved it, no one or the
/// person whose device signed it in this room, by a proof that holds now
/// ([`super::saved_shortcut::proven_saver`]). A shortcut the room already
/// holds, unchanged, is not written, so one saved before proofs existed stays
/// as it is: it runs by hand and never arms. A document received with nothing
/// held before it is not a change to judge; what it names is judged where it
/// counts, each time a trigger arms.
fn saved_shortcuts_proven(
    before: Option<&AutoCommit>,
    candidate: &AutoCommit,
    room: &str,
    now_ns: u64,
) -> Result<(), RoomRefusal> {
    use tilo_desktop_model::projection::workspace_shortcuts_of;
    let Some(before) = before else {
        return Ok(());
    };
    let held = workspace_shortcuts_of(before);
    let mut written = workspace_shortcuts_of(candidate)
        .into_iter()
        .filter(|saved| !held.contains(saved))
        .filter_map(|saved| saved.decode().ok())
        .filter(|shortcut| shortcut.saved_by.is_some())
        .peekable();
    if written.peek().is_none() {
        return Ok(());
    }
    let revocations = super::saved_shortcut::room_revocations(candidate);
    for shortcut in written {
        if super::saved_shortcut::proven_saver(room, &shortcut, &revocations, now_ns)
            != shortcut.saved_by
        {
            return Err(RoomRefusal::ShortcutAuthorship(shortcut.id));
        }
    }
    Ok(())
}

/// Whether a sender that is not the governance writer may make undoable
/// changes on the room (`true`), or only read it and so record its own
/// effect receipts and give its own shares (`false`), decided against the
/// policy already held — never the one the batch brings.
fn may_write(
    before: Option<&AutoCommit>,
    room: &str,
    source: &Writer<'_>,
    now_ns: u64,
) -> Result<bool, RoomRefusal> {
    let Standing::Member { person, foreign } = source.standing else {
        return Ok(true);
    };
    let before = before.ok_or(RoomRefusal::GovernanceMissing)?;
    let policy = tilo_desktop_model::projection::room_policy_of(before, room);
    let person = hex::encode(person);
    if policy.active_role(&person).is_none() {
        return Err(RoomRefusal::WriteForbidden);
    }
    let bound = tilo_action_policy::roles::foreign_device_bound();
    let authority = policy.person_on_device(&person, foreign, &bound);
    let cx = Context::at_secs(now_ns / 1_000_000_000);
    let allows = |kind| authority.decide(&TreePath::room(room), kind, &cx).answer == Answer::Allow;
    if allows(ActionKind::Undoable) {
        Ok(true)
    } else if allows(ActionKind::Read) {
        Ok(false)
    } else {
        Err(RoomRefusal::WriteForbidden)
    }
}

#[cfg(test)]
mod tests;
