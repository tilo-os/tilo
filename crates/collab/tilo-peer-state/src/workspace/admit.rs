//! The one admission rule for a workspace document a peer sends (`SyncResp`).
//!
//! The native peer ([`crate::StatePeer`]) and the shell's workspace tree both
//! receive the same workspace document — owner, capabilities, identities, media
//! policy, coverage, durability, receipts and window contents — so both admit it
//! here. What differs between them is only data: how the receiver identifies
//! the sender ([`Writer`]) and whose workspace it is. Each caller keeps its own
//! storage and projection.
//!
//! The whole batch is refused or the whole candidate is returned: a partial
//! merge would advance local heads past refused operations.
use automerge::AutoCommit;
use tilo_action_policy::{ActionKind, Answer, Context, TreePath};

use super::media;

/// How the receiver knows the sender's standing in the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// The receiver resolved the sender to this person root, from a device
    /// held to the foreign-device bound or not. The batch needs undoable
    /// changes on the workspace allowed under the policy the receiver already
    /// holds.
    Member { person: [u8; 32], foreign: bool },
    /// The receiver's transport decided this sender may change the workspace before
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

    /// Whether the sender is a device of `owner`: the workspace's governance is
    /// theirs to write, from any device their root certifies. The receiver
    /// knows the owner from what it joined through, never from the batch,
    /// so a first answer cannot make its sender the owner.
    fn governs(&self, owner: &[u8; 32]) -> bool {
        self.person() == Some(owner)
    }
}

/// Everything [`admit_workspace_changes`] decides a batch against.
#[derive(Clone, Copy, Debug)]
pub struct WorkspaceAdmission<'a> {
    /// The workspace document the receiver holds now; the candidate is built on it.
    pub before: Option<&'a AutoCommit>,
    /// The workspace id as governance and capability scopes spell it.
    pub workspace: &'a str,
    pub source: Writer<'a>,
    /// The workspace's owner: the person the workspace was made for.
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
pub struct AdmittedWorkspace {
    pub candidate: AutoCommit,
    pub media: Option<media::WorkspaceMediaV1>,
    /// The candidate is a newer base ([`super::base`]) replacing the
    /// document held, not that document with the batch applied.
    pub rebased: bool,
}

/// Why a batch was refused. Display strings are the peer-state messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceRefusal {
    /// The sender is not the workspace's owner and nothing is held to check
    /// its membership against.
    GovernanceMissing,
    /// The sender may not change the workspace.
    WriteForbidden,
    /// The batch does not decode, or isolated admission refused it.
    Malformed(String),
    /// The document names a different workspace than the one it answers.
    DeclaredWorkspaceMismatch,
    /// The batch changes governance the writer's role does not permit, or
    /// governance no operation of its own names.
    GovernanceWriter,
    /// A member's membership change is not theirs to make.
    Membership(String),
    /// The batch rewrites an effect receipt.
    ReceiptImmutable,
    /// The batch redeems an invitation the workspace already redeemed, or
    /// takes a redemption back.
    RedemptionImmutable,
    /// The batch records an effect receipt, or anchors archived ones,
    /// outside its writer's own slice ([`super::effect_slices_kept`]).
    ReceiptWriter,
    /// The batch keeps, rewrites or clears a write refusal outside its
    /// writer's own device's slice, or one no such device signed
    /// ([`super::refusal_slices_kept`]).
    RefusalWriter,
    Coverage(String),
    /// The batch rewinds a durable receipt.
    Durability(String),
    /// The media policy is malformed or its transition is not permitted.
    Media(media::MediaPolicyError),
    /// The batch adds or changes the shortcut `0` naming who saved it
    /// without that person's device's proof ([`super::saved_shortcut`]).
    ShortcutAuthorship(String),
}

impl std::fmt::Display for WorkspaceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GovernanceMissing => f.write_str("workspace governance missing"),
            Self::WriteForbidden => f.write_str("workspace write forbidden"),
            Self::Malformed(error)
            | Self::Coverage(error)
            | Self::Durability(error)
            | Self::Membership(error) => f.write_str(error),
            Self::DeclaredWorkspaceMismatch => f.write_str("declared workspace mismatch"),
            Self::GovernanceWriter => f.write_str("governance is not this writer's to change"),
            Self::ReceiptImmutable => f.write_str("effect receipt is immutable"),
            Self::RedemptionImmutable => {
                f.write_str("governed invitation has conflicting redemption receipts")
            }
            Self::ReceiptWriter => f.write_str("effect receipt is not its writer's own"),
            Self::RefusalWriter => f.write_str("write refusal is not its recorder's own"),
            Self::Media(error) => write!(f, "{error}"),
            Self::ShortcutAuthorship(shortcut) => write!(
                f,
                "the shortcut `{shortcut}` names who saved it without their device's proof"
            ),
        }
    }
}

impl std::error::Error for WorkspaceRefusal {}

/// Admit `changes` to `input.before`: build the candidate and run every check a
/// workspace document must pass, in one place for every receiver.
///
/// 1. The sender may make undoable changes on the workspace, unless it is the
///    owner's device (who may grant itself anything) — [`Standing`].
/// 2. The batch decodes under the shared incoming limits ([`super::candidate`]).
/// 3. Then [`check_candidate`].
///
/// # Errors
///
/// The first [`WorkspaceRefusal`] that applies; nothing is admitted then.
pub fn admit_workspace_changes(
    input: WorkspaceAdmission<'_>,
    changes: &[Vec<u8>],
) -> Result<AdmittedWorkspace, WorkspaceRefusal> {
    let governs = input.source.governs(input.owner);
    let writes = governs || may_write(input.before, input.workspace, &input.source, input.now_ns)?;
    match super::base::lineage(input.before, changes) {
        super::base::Lineage::Same { changes, replaced } => {
            // Changes waiting in the document that the batch shows to be of
            // another history go before the batch is applied.
            let pruned = super::base::pruned(input.before, replaced);
            let mut candidate = super::candidate(pruned.as_ref().or(input.before), &changes)
                .map_err(WorkspaceRefusal::Malformed)?;
            // One who only reads the workspace records only their own receipts
            // and gives only their own shares.
            if !writes && !super::only_reader_writes_differ(input.before, &candidate) {
                return Err(WorkspaceRefusal::WriteForbidden);
            }
            // What a batch changes in the workspace's governance is what its
            // operations say it changes, each of them its author's to make: a
            // membership change replays under its writer's grants
            // ([`super::membership`]), and a governance change is the row its
            // operation names, judged by its author's role.
            let grown = crate::members::growth(input.before, &candidate)
                .map_err(WorkspaceRefusal::Membership)?;
            let governed = if grown > 0 && !governs {
                super::membership::admit_operations(
                    input.before,
                    &candidate,
                    &changes,
                    input.workspace,
                    input.now_ns,
                )
                .map_err(membership)?;
                true
            } else if !governs && !super::governance_unchanged(input.before, &candidate) {
                membership_authorized(&input, &changes)?;
                true
            } else {
                false
            };
            let media = checked(&input, &candidate, governed)?;
            crate::members::settle(&mut candidate, input.workspace)
                .map_err(WorkspaceRefusal::Malformed)?;
            Ok(AdmittedWorkspace {
                candidate,
                media,
                rebased: false,
            })
        }
        super::base::Lineage::Newer(changes) if governs => {
            let mut candidate =
                super::candidate(None, &changes).map_err(WorkspaceRefusal::Malformed)?;
            crate::members::settle(&mut candidate, input.workspace)
                .map_err(WorkspaceRefusal::Malformed)?;
            // The receiver projects what it owns onto the base it adopts, so a
            // base leaving out or changing its entries holds only until then.
            let media = checked(&input, &candidate, false)?;
            Ok(AdmittedWorkspace {
                candidate,
                media,
                rebased: true,
            })
        }
        super::base::Lineage::Newer(_) => Err(WorkspaceRefusal::GovernanceWriter),
    }
}

/// A member's batch that changes governance, judged as a membership change
/// where its writer made it ([`super::membership`]). Returns whether it was
/// admitted as one.
fn membership_authorized(
    input: &WorkspaceAdmission<'_>,
    changes: &[Vec<u8>],
) -> Result<bool, WorkspaceRefusal> {
    let (Some(before), Standing::Member { person, foreign }) =
        (input.before, input.source.standing)
    else {
        return Err(WorkspaceRefusal::GovernanceWriter);
    };
    let (at, after) = super::membership::position(before, changes).map_err(membership)?;
    super::membership::authorize(
        input.workspace,
        (&person, input.source.key, foreign),
        &at,
        changes,
        &after,
        input.now_ns,
    )
    .map_err(membership)?;
    Ok(true)
}

fn membership(refusal: super::membership::MembershipRefusal) -> WorkspaceRefusal {
    match refusal {
        super::membership::MembershipRefusal::NotMembership => WorkspaceRefusal::GovernanceWriter,
        refusal => WorkspaceRefusal::Membership(refusal.to_string()),
    }
}

/// Check a candidate workspace document, however it was built: from
/// `input.before` and a batch ([`admit_workspace_changes`]), or as a whole
/// checkpoint replacing it. Returns the candidate's media policy for the caller
/// to bind.
///
/// 1. The document names this workspace ([`super::declared_workspace_mismatches`]).
/// 2. Governance changed beyond what an operation of the batch or its writer's
///    own membership change admits is refused ([`WorkspaceRefusal::GovernanceWriter`]).
/// 3. Effect receipts are immutable ([`super::immutable_receipts`]), and
///    one not from the owner's devices records only its writer's own
///    ([`super::effect_slices_kept`]).
/// 4. Every invitation keeps the one redemption the workspace recorded for it
///    ([`super::redemptions_kept`]).
/// 5. Coverage and durable receipts never rewind
///    ([`crate::coverage::monotonic`], [`crate::durability::monotonic`]).
/// 6. The media policy is well formed and its transition is permitted
///    ([`media::validate_transition`]).
/// 7. A shortcut names as who saved it only a proven saver
///    ([`super::saved_shortcut`]).
///
/// # Errors
///
/// The first [`WorkspaceRefusal`] that applies.
fn checked(
    input: &WorkspaceAdmission<'_>,
    candidate: &AutoCommit,
    governed: bool,
) -> Result<Option<media::WorkspaceMediaV1>, WorkspaceRefusal> {
    let WorkspaceAdmission {
        before,
        workspace,
        source,
        owner,
        now_ns,
    } = *input;
    let governs = source.governs(owner);
    let declared = uuid::Uuid::parse_str(workspace)
        .map_err(|_| WorkspaceRefusal::DeclaredWorkspaceMismatch)?;
    if super::declared_workspace_mismatches(candidate, declared) {
        return Err(WorkspaceRefusal::DeclaredWorkspaceMismatch);
    }
    if !governs && !governed && !super::governance_unchanged(before, candidate) {
        return Err(WorkspaceRefusal::GovernanceWriter);
    }
    if !governs {
        // The owner's devices relay what members recorded; anyone else
        // records only their own.
        if !super::effect_slices_kept(before, candidate, source.person()) {
            return Err(WorkspaceRefusal::ReceiptWriter);
        }
        // A device's verdicts on other devices' writes are its own to keep
        // and clear, as its receipts are.
        // ignored-failure: a key that is not a device key keeps no slice of
        // its own, so every refusal it writes is refused rather than trusted.
        let sender = hex::decode(source.key)
            .ok()
            .and_then(|key| <[u8; 32]>::try_from(key.as_slice()).ok());
        if !super::refusal_slices_kept(before, candidate, source.person(), sender.as_ref()) {
            return Err(WorkspaceRefusal::RefusalWriter);
        }
    }
    // Every writer, the owner's devices included: it relays and merges what members
    // write, so who sent a batch says nothing about who saved a shortcut.
    saved_shortcuts_proven(before, candidate, workspace, now_ns)?;
    if !super::immutable_receipts(before, candidate) {
        return Err(WorkspaceRefusal::ReceiptImmutable);
    }
    // One redemption per invitation for every writer, the owner's devices
    // included: whichever device answered, it admits one person at a time.
    if !super::redemptions_kept(before, candidate) {
        return Err(WorkspaceRefusal::RedemptionImmutable);
    }
    crate::coverage::monotonic(before, candidate).map_err(WorkspaceRefusal::Coverage)?;
    crate::durability::monotonic(before, candidate).map_err(WorkspaceRefusal::Durability)?;
    media::validate_transition(before, candidate, media::TransitionMode::Admission)
        .map_err(WorkspaceRefusal::Media)
}

/// A shortcut a batch adds or changes names, as who saved it, no one or the
/// person whose device signed it in this workspace, by a proof that holds now
/// ([`super::saved_shortcut::proven_saver`]). A shortcut the workspace already
/// holds, unchanged, is not written, so one saved before proofs existed stays
/// as it is: it runs by hand and never arms. A document received with nothing
/// held before it is not a change to judge; what it names is judged where it
/// counts, each time a trigger arms.
fn saved_shortcuts_proven(
    before: Option<&AutoCommit>,
    candidate: &AutoCommit,
    workspace: &str,
    now_ns: u64,
) -> Result<(), WorkspaceRefusal> {
    use tilo_desktop_model::projection::workspace_shortcuts_of;
    let Some(before) = before else {
        return Ok(());
    };
    let held = workspace_shortcuts_of(before);
    let mut written = workspace_shortcuts_of(candidate)
        .into_iter()
        .filter(|saved| !held.contains(saved))
        // ignored-failure: a shortcut that does not decode proves no saver, so admits nothing
        .filter_map(|saved| saved.decode().ok())
        .filter(|shortcut| shortcut.saved_by.is_some())
        .peekable();
    if written.peek().is_none() {
        return Ok(());
    }
    let revocations = super::saved_shortcut::workspace_revocations(candidate);
    for shortcut in written {
        if super::saved_shortcut::proven_saver(workspace, &shortcut, &revocations, now_ns)
            != shortcut.saved_by
        {
            return Err(WorkspaceRefusal::ShortcutAuthorship(shortcut.id));
        }
    }
    Ok(())
}

/// Whether a sender that is not the workspace's owner may make undoable
/// changes on the workspace (`true`), or only read it and so record its own
/// effect receipts and give its own shares (`false`), decided against the
/// policy already held — never the one the batch brings.
fn may_write(
    before: Option<&AutoCommit>,
    workspace: &str,
    source: &Writer<'_>,
    now_ns: u64,
) -> Result<bool, WorkspaceRefusal> {
    let Standing::Member { person, foreign } = source.standing else {
        return Ok(true);
    };
    let before = before.ok_or(WorkspaceRefusal::GovernanceMissing)?;
    let policy = crate::workspace::policy_of(before, workspace);
    let person = hex::encode(person);
    if policy.active_role(&person).is_none() {
        return Err(WorkspaceRefusal::WriteForbidden);
    }
    let bound = tilo_action_policy::roles::foreign_device_bound();
    let authority = policy.person_on_device(&person, foreign, &bound);
    let cx = Context::at_secs(now_ns / 1_000_000_000);
    let allows = |kind| {
        authority
            .decide(&TreePath::workspace(workspace), kind, &cx)
            .answer
            == Answer::Allow
    };
    if allows(ActionKind::Undoable) {
        Ok(true)
    } else if allows(ActionKind::Read) {
        Ok(false)
    } else {
        Err(WorkspaceRefusal::WriteForbidden)
    }
}

#[cfg(test)]
mod tests;
