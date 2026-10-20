//! The one admission rule for a room document a peer sends (`SyncResp`).
//!
//! The native peer ([`crate::StatePeer`]) and the shell's workspace tree both
//! receive the same room document — owner, capabilities, identities, media
//! policy, coverage, durability, receipts and window contents — so both admit it
//! here. What differs between them is only data: how the receiver identifies
//! the sender ([`Writer`]), who it pinned as the governance writer
//! ([`AuthorityPin`]) and how the room's window contents are owned
//! ([`ContentOwnership`]). Each caller keeps its own storage and projection.
//!
//! The whole batch is refused or the whole candidate is returned: a partial
//! merge would advance local heads past refused operations.
use automerge::AutoCommit;
use tilo_action_policy::{ActionKind, Answer, Context, TreePath};

use super::media;

/// Who may change a room's governance, as the receiver pinned it from local,
/// authenticated knowledge — an invite's signed authority, the person's own
/// devices, or this machine. Never derived from the batch being admitted, so a
/// first answer cannot make its sender the governor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityPin<'a> {
    /// Exactly this transport key: the authority device an invite signed.
    Device(&'a str),
    /// Any device the transport authenticated as this person root.
    Person(&'a [u8; 32]),
    /// This machine governs the room: no remote sender may change governance.
    Local,
    /// No governor is known (the room was joined without an invite naming
    /// one): nobody may change governance until an authority is pinned.
    Unpinned,
}

impl AuthorityPin<'_> {
    /// Whether `writer` is the pinned governance writer.
    #[must_use]
    pub fn admits(&self, writer: &Writer<'_>) -> bool {
        match *self {
            Self::Device(key) => key == writer.key,
            Self::Person(person) => writer.person() == Some(person),
            Self::Local | Self::Unpinned => false,
        }
    }
}

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
    /// The authenticated sender key, spelt the way [`AuthorityPin::Device`]
    /// and the topology registry name it.
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
}

/// Who owns the entries of a room's window contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentOwnership {
    /// The `contents_owners_v1` registry names each namespace's owner; a room
    /// without one is owned by the pinned authority device
    /// ([`crate::topology::validate`]).
    Registry,
    /// Each machine owns the entries minted under its id salt. The receiver
    /// refuses a batch that rewrites an entry carrying `local_salt` that both
    /// sides already hold
    /// ([`tilo_desktop_model::projection::changed_owned_entries`]).
    Salted { local_salt: u16 },
}

/// Everything [`admit_room_changes`] decides a batch against.
#[derive(Clone, Copy, Debug)]
pub struct RoomAdmission<'a> {
    /// The room document the receiver holds now; the candidate is built on it.
    pub before: Option<&'a AutoCommit>,
    /// The room id as governance and capability scopes spell it.
    pub room: &'a str,
    pub source: Writer<'a>,
    pub authority: AuthorityPin<'a>,
    pub contents: ContentOwnership,
    /// Seconds since the Unix epoch, for grant expiry.
    pub caps_now: u64,
}

/// A batch every check admitted. The caller binds a returned media policy to
/// its own authority record before installing the candidate.
#[derive(Debug)]
pub struct AdmittedRoom {
    pub candidate: AutoCommit,
    pub media: Option<media::RoomMediaV1>,
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
    /// The batch changes governance and its sender is not the pinned writer.
    GovernanceWriter,
    /// The batch changes window contents its sender does not own.
    Topology(String),
    /// The batch puts window contents into a room whose layout is personal:
    /// each member's windows stay on their device
    /// ([`tilo_desktop_model::projection::RoomLayout::Personal`]). Taking
    /// one's own out is admitted.
    PersonalLayout,
    /// The batch rewrites `n` window entries this machine owns.
    OwnedContents(usize),
    /// The batch rewrites an effect receipt.
    ReceiptImmutable,
    /// The batch rewinds checkpoint coverage.
    Coverage(String),
    /// The batch rewinds a durable receipt.
    Durability(String),
    /// The media policy is malformed or its transition is not permitted.
    Media(media::MediaPolicyError),
}

impl std::fmt::Display for RoomRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GovernanceMissing => f.write_str("room governance missing"),
            Self::WriteForbidden => f.write_str("room write forbidden"),
            Self::Malformed(error)
            | Self::Topology(error)
            | Self::Coverage(error)
            | Self::Durability(error) => f.write_str(error),
            Self::DeclaredRoomMismatch => f.write_str("declared room mismatch"),
            Self::GovernanceWriter => f.write_str("governance writer is not pinned authority"),
            Self::PersonalLayout => {
                f.write_str("the room's layout is personal: it takes no member's windows")
            }
            Self::OwnedContents(entries) => write!(
                f,
                "owned-content conflict: the batch rewrites {entries} window entries this \
                 machine owns"
            ),
            Self::ReceiptImmutable => f.write_str("effect receipt is immutable"),
            Self::Media(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RoomRefusal {}

/// Admit `changes` to `input.before`: build the candidate and run every check a
/// room document must pass, in one place for every receiver.
///
/// 1. The sender may make undoable changes on the room, unless it is the
///    pinned governance writer (who may grant itself anything) — [`Standing`].
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
    if !input.authority.admits(&input.source) {
        may_write(input.before, input.room, &input.source, input.caps_now)?;
    }
    let candidate = super::candidate(input.before, changes).map_err(RoomRefusal::Malformed)?;
    let media = check_candidate(&input, &candidate)?;
    Ok(AdmittedRoom { candidate, media })
}

/// Check a candidate room document against `input.before`, however it was
/// built: from `input.before` and a batch ([`admit_room_changes`]), or as a
/// whole checkpoint replacing it. Returns the candidate's media policy for the
/// caller to bind.
///
/// 1. The document names this room ([`super::declared_room_mismatches`]).
/// 2. A governance change — owner, capabilities, identities, media,
///    coverage, receipts, durability or import proposals — comes from the
///    pinned writer ([`super::governance_unchanged`]).
/// 3. Window contents change only by their owner ([`ContentOwnership`]),
///    and a member puts none into a room whose layout is personal; the
///    layout scope changes only by the pinned writer.
/// 4. Effect receipts are immutable ([`super::immutable_receipts`]).
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
    let RoomAdmission {
        before,
        room,
        source,
        authority,
        contents,
        caps_now: _,
    } = *input;
    let governs = authority.admits(&source);
    let declared = uuid::Uuid::parse_str(room).map_err(|_| RoomRefusal::DeclaredRoomMismatch)?;
    if super::declared_room_mismatches(candidate, declared) {
        return Err(RoomRefusal::DeclaredRoomMismatch);
    }
    if !governs && !super::governance_unchanged(before, candidate) {
        return Err(RoomRefusal::GovernanceWriter);
    }
    if !governs {
        personal_layout_kept(before, candidate)?;
    }
    match contents {
        ContentOwnership::Registry => {
            let AuthorityPin::Device(pinned) = authority else {
                return Err(RoomRefusal::Topology(
                    "topology registry requires a pinned authority device".into(),
                ));
            };
            crate::topology::validate(before, candidate, source.key, pinned)
                .map_err(RoomRefusal::Topology)?;
        }
        ContentOwnership::Salted { local_salt } => {
            let rewritten = tilo_desktop_model::projection::changed_owned_entries(
                before, candidate, local_salt,
            );
            if rewritten > 0 {
                return Err(RoomRefusal::OwnedContents(rewritten));
            }
        }
    }
    if !super::immutable_receipts(before, candidate) {
        return Err(RoomRefusal::ReceiptImmutable);
    }
    crate::coverage::monotonic(before, candidate).map_err(RoomRefusal::Coverage)?;
    crate::durability::monotonic(before, candidate).map_err(RoomRefusal::Durability)?;
    media::validate_transition(before, candidate, media::TransitionMode::Admission)
        .map_err(RoomRefusal::Media)
}

/// A batch from a sender that is not the governance writer leaves the room's
/// layout scope as its governor set it, and in a room whose layout is
/// personal ([`tilo_desktop_model::projection::RoomLayout::Personal`]), as
/// the room held it before, puts no window contents in: a member may only
/// take its own out (which entries are whose is [`ContentOwnership`]'s to
/// decide).
fn personal_layout_kept(
    before: Option<&AutoCommit>,
    candidate: &AutoCommit,
) -> Result<(), RoomRefusal> {
    use tilo_desktop_model::projection::{room_layout, room_layout_choice};
    if before.and_then(room_layout_choice) != room_layout_choice(candidate) {
        return Err(RoomRefusal::GovernanceWriter);
    }
    if before.is_some_and(|before| room_layout(before).is_personal())
        && crate::topology::adds_to(before, candidate).map_err(RoomRefusal::Topology)?
    {
        return Err(RoomRefusal::PersonalLayout);
    }
    Ok(())
}

/// Undoable changes on the room for a sender that is not the governance
/// writer, decided against the policy already held — never the one the batch
/// brings.
fn may_write(
    before: Option<&AutoCommit>,
    room: &str,
    source: &Writer<'_>,
    caps_now: u64,
) -> Result<(), RoomRefusal> {
    let Standing::Member { person, foreign } = source.standing else {
        return Ok(());
    };
    let before = before.ok_or(RoomRefusal::GovernanceMissing)?;
    let policy = tilo_desktop_model::projection::room_policy_of(before, room);
    let person = hex::encode(person);
    if policy.active_role(&person).is_none() {
        return Err(RoomRefusal::WriteForbidden);
    }
    let bound = tilo_action_policy::roles::foreign_device_bound();
    let authority = policy.person_on_device(&person, foreign, &bound);
    let cx = Context::at_secs(caps_now);
    if authority
        .decide(&TreePath::room(room), ActionKind::Undoable, &cx)
        .answer
        == Answer::Allow
    {
        Ok(())
    } else {
        Err(RoomRefusal::WriteForbidden)
    }
}

#[cfg(test)]
mod tests;
