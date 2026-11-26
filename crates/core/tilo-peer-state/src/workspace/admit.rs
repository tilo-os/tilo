//! The one admission rule for a workspace document a peer sends (`SyncResp`).
//!
//! The native peer ([`crate::StatePeer`]) and the shell's workspace tree both
//! receive the same workspace document — owner, capabilities, identities,
//! coverage, durability, receipts and window contents — so both admit it
//! here. What differs between them is only data: how the receiver identifies
//! the sender ([`Writer`]) and whose workspace it is. Each caller keeps its own
//! storage and projection.
//!
//! The whole batch is refused or the whole candidate is returned: a partial
//! merge would advance local heads past refused operations.
use automerge::AutoCommit;
use tilo_action_policy::{ActionKind, Answer, Context, TreePath};

/// How the receiver knows the sender's access in the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SenderAccess {
    /// The receiver resolved the sender to this person root, from a device
    /// held to the foreign-device bound or not. The batch needs undoable
    /// changes on the workspace allowed under the policy the receiver already
    /// holds.
    Member { person: [u8; 32], foreign: bool },
    /// The receiver's transport decided this sender may change the workspace before
    /// it delivered the batch, and the receiver has no roster entry of its own to
    /// decide it again (the shell's network check, `peer_workspace_allows`). Every
    /// other check still applies.
    TransportAuthorized,
    /// The sender holds no role in the workspace: a device enrolling
    /// itself, or a holder that is no member and so carries what members
    /// signed without writing anything of its own. What it may write is its
    /// own admission and nothing else ([`enrolling_writes`]).
    Enrolling,
}

/// The sender of a batch, as the receiver's transport authenticated it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Writer<'a> {
    /// The authenticated sender key, spelt the way the topology registry
    /// names it.
    pub key: &'a str,
    pub access: SenderAccess,
}

impl Writer<'_> {
    fn person(&self) -> Option<&[u8; 32]> {
        match &self.access {
            SenderAccess::Member { person, .. } => Some(person),
            SenderAccess::TransportAuthorized | SenderAccess::Enrolling => None,
        }
    }

    /// Whether the sender keeps the workspace's history for its members: a
    /// device of its owner, or of one of `administrators`, from a device of
    /// their own, each holding that role on the signed member list. They
    /// relay what members recorded and send the workspace's compacted base.
    fn keeps_history(&self, owner: &[u8; 32], administrators: &[[u8; 32]]) -> bool {
        matches!(
            self.access,
            SenderAccess::Member { person, foreign: false }
                if person == *owner || administrators.contains(&person)
        )
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
    /// The workspace's owner, as the governance held names them.
    pub owner: &'a [u8; 32],
    /// The workspace's administrators, by root, as the governance held
    /// names them ([`Writer::keeps_history`]).
    pub administrators: &'a [[u8; 32]],
    /// The devices whose place on the signed member list, or among the
    /// workspace's holders, anchors a first governance this replica takes in
    /// with nothing held before it: its own device and the device it was
    /// joined through ([`first_member_list`]).
    pub anchors: &'a [[u8; 32]],
    /// The instant the batch is judged at, in nanoseconds since the Unix
    /// epoch: grant expiry, and the certificate a saved shortcut's proof
    /// carries. Not whole seconds: a certificate issued earlier in this
    /// second is already valid.
    pub now_ns: u64,
}

/// A batch every check admitted.
#[derive(Debug)]
pub struct AdmittedWorkspace {
    pub candidate: AutoCommit,
    /// The candidate is a newer base ([`super::base`]) replacing the
    /// document held, not that document with the batch applied.
    pub rebased: bool,
}

/// Why a batch was refused. Display strings are the peer-state messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceRefusal {
    /// Nothing is held to check the sender's membership against.
    RulesMissing,
    /// The sender may not change the workspace.
    WriteForbidden,
    /// The batch does not decode, or isolated admission refused it.
    Malformed(String),
    /// The document names a different workspace than the one it answers.
    DeclaredWorkspaceMismatch,
    /// The batch changes governance the writer's role does not permit, or
    /// governance no operation of its own names.
    GovernanceWriter,
    /// The batch starts the workspace over from another founding: a document
    /// founded afresh under the workspace's id, by a device that never held
    /// what the workspace holds ([`super::base::Lineage::Refounded`]).
    Refounded,
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
    /// The batch keeps, rewrites or clears a checkpoint check outside its
    /// writer's own device's slice, or one no such device signed
    /// ([`super::checkpoint_check_slices_kept`]).
    CheckpointCheckWriter,
    Coverage(String),
    /// A change erases or lowers the cut of a device's removal
    /// ([`crate::removals::monotonic`]), or of a change that took standing
    /// away ([`crate::standing::monotonic`]).
    Removals(String),
    /// The batch rewinds a durable receipt.
    Durability(String),
    /// The batch adds or changes the shortcut `0` naming who saved it
    /// without that person's device's proof ([`super::saved_shortcut`]).
    ShortcutAuthorship(String),
    /// The batch starts the workspace's history where no device takes it
    /// from: a base no device of its owner or an administrator signed, or,
    /// for a device holding nothing of it yet, anything but its founding or
    /// such a base ([`super::compaction`]).
    Compaction(super::compaction::CompactionRefusal),
}

impl std::fmt::Display for WorkspaceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RulesMissing => f.write_str("the workspace's rules are not here yet"),
            Self::WriteForbidden => f.write_str("workspace write forbidden"),
            Self::Malformed(error)
            | Self::Coverage(error)
            | Self::Removals(error)
            | Self::Durability(error)
            | Self::Membership(error) => f.write_str(error),
            Self::DeclaredWorkspaceMismatch => f.write_str("declared workspace mismatch"),
            Self::GovernanceWriter => f.write_str("governance is not this writer's to change"),
            Self::Refounded => f.write_str(
                "the workspace was founded once already: a document founded again under its id \
                 replaces nothing it holds",
            ),
            Self::ReceiptImmutable => f.write_str("effect receipt is immutable"),
            Self::RedemptionImmutable => {
                f.write_str("an invitation has conflicting redemption receipts")
            }
            Self::ReceiptWriter => f.write_str("effect receipt is not its writer's own"),
            Self::RefusalWriter => f.write_str("write refusal is not its recorder's own"),
            Self::CheckpointCheckWriter => f.write_str("checkpoint check is not its device's own"),
            Self::Compaction(refusal) => refusal.fmt(f),
            Self::ShortcutAuthorship(shortcut) => write!(
                f,
                "the shortcut `{shortcut}` names who saved it without their device's proof"
            ),
        }
    }
}

impl WorkspaceRefusal {
    /// The class of the refusal, as the batch's writer is told it.
    #[must_use]
    pub const fn reason(&self) -> neighbour_wire::RefusalReason {
        use neighbour_wire::RefusalReason;
        match self {
            Self::RulesMissing => RefusalReason::MissingDependency,
            Self::WriteForbidden
            | Self::GovernanceWriter
            | Self::Membership(_)
            | Self::ReceiptWriter
            | Self::RefusalWriter
            | Self::CheckpointCheckWriter
            | Self::Compaction(_)
            | Self::ShortcutAuthorship(_) => RefusalReason::Forbidden,
            Self::Malformed(_) | Self::DeclaredWorkspaceMismatch | Self::Coverage(_) => {
                RefusalReason::Malformed
            }
            Self::Refounded
            | Self::ReceiptImmutable
            | Self::RedemptionImmutable
            | Self::Removals(_)
            | Self::Durability(_) => RefusalReason::Conflict,
        }
    }
}

impl std::error::Error for WorkspaceRefusal {}

/// Why [`admit_workspace_changes`] answered nothing. Only [`Self::Refused`] is
/// a verdict on the batch; [`Self::Device`] is this device failing to decide it
/// (an isolated worker crashed, overran its deadline or never started), which
/// says nothing about what the sender wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionFailure {
    Refused(WorkspaceRefusal),
    Device(String),
}
impl AdmissionFailure {
    /// Whether this device failed to decide the batch, rather than refusing it.
    #[must_use]
    pub fn is_device_failure(&self) -> bool {
        matches!(self, Self::Device(_))
    }
}
impl std::fmt::Display for AdmissionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Device(reason) => f.write_str(reason),
        }
    }
}
impl std::error::Error for AdmissionFailure {}
impl From<WorkspaceRefusal> for AdmissionFailure {
    fn from(refusal: WorkspaceRefusal) -> Self {
        Self::Refused(refusal)
    }
}

/// Admit `changes` to `input.before`: build the candidate and run every check a
/// workspace document must pass, in one place for every receiver.
///
/// 1. With nothing held, the batch is a first governance ([`first_member_list`]).
/// 2. A sender holding no role writes only its own admission
///    ([`enrolling_writes`]), and so does one who only reads, beyond their
///    own receipts and shares.
/// 3. Otherwise the sender may make undoable changes on the workspace, or
///    only what one who reads it writes ([`may_write`]); every governance
///    change is an operation its author's role permits, the owner's included.
/// 4. The batch decodes under the shared incoming limits ([`super::candidate`]).
/// 5. Then [`checked`].
///
/// # Errors
///
/// The first [`WorkspaceRefusal`] that applies, or [`AdmissionFailure::Device`]
/// when this device could not decide the batch; nothing is admitted then.
pub fn admit_workspace_changes(
    input: WorkspaceAdmission<'_>,
    changes: &[Vec<u8>],
) -> Result<AdmittedWorkspace, AdmissionFailure> {
    if input.before.is_none() {
        return first_member_list(&input, changes);
    }
    if matches!(input.source.access, SenderAccess::Enrolling) {
        return enrolling_writes(&input, changes);
    }
    let writes = may_write(input.before, input.workspace, &input.source, input.now_ns)?;
    match super::base::lineage(input.before, changes) {
        super::base::Lineage::Same { changes, replaced } => {
            // Changes waiting in the document that the batch shows to be of
            // another history go before the batch is applied.
            let pruned = super::base::pruned(input.before, replaced);
            let candidate = super::candidate(pruned.as_ref().or(input.before), &changes)?;
            // One who only reads the workspace records only their own receipts
            // and gives only their own shares, and writes their own way in as
            // a device holding no role does: a reader's device redeeming its
            // invitation, or listing itself.
            if !writes && !super::only_reader_writes_differ(input.before, &candidate) {
                return enrolling_writes(&input, &changes);
            }
            // What a batch changes in the workspace's governance is what its
            // operations say it changes, each of them its author's to make: a
            // membership change replays under its writer's grants
            // ([`super::membership`]), and a governance change is the row its
            // operation names, judged by its author's role. The owner's
            // devices write theirs the same way.
            let grown = crate::members::growth(input.before, &candidate)
                .map_err(WorkspaceRefusal::Membership)?;
            let hosted = if grown > 0 {
                super::membership::admit_operations(
                    input.before,
                    &candidate,
                    &changes,
                    input.workspace,
                    (input.now_ns, keeps_checkpoints(&input)),
                )
                .map_err(membership)?;
                true
            } else if !super::governance_unchanged(input.before, &candidate) {
                // A change to the workspace's governance is an operation's: a
                // batch that changes it without one changes nothing, but for
                // the checkpoints a keeper publishes as it publishes them.
                let written: Vec<automerge::Change> = changes
                    .iter()
                    .filter_map(|bytes| automerge::Change::try_from(bytes.as_slice()).ok())
                    .collect();
                let checkpoints = input.before.is_some_and(|before| {
                    super::changed_rows_by(before, &candidate, &written, true)
                        == [super::GovernanceRow::Checkpoints]
                });
                if !(checkpoints && keeps_checkpoints(&input)) {
                    return Err(WorkspaceRefusal::GovernanceWriter.into());
                }
                true
            } else {
                false
            };
            checked(&input, &candidate, hosted)?;
            Ok(AdmittedWorkspace {
                candidate,
                rebased: false,
            })
        }
        super::base::Lineage::Newer { base, changes } => {
            let candidate = super::candidate_on(*base, &changes)?;
            // A newer base is its maker's signed compaction, whoever carried
            // it: a device of the owner's or of an administrator's, as the
            // base's own member list names them. A base holds the workspace as
            // it stands, so it carries every governance change made since the
            // receiver's copy: an admission, a hand-over. It is taken whole
            // when it keeps every membership operation the receiver holds and
            // each one it adds proves its author, as a first governance is.
            // An administrator's base changes what the operations it adds say
            // it changes, each its author's to make, as in a batch.
            let maker = super::compaction::compacted_by(&candidate, input.workspace, input.owner)
                .map_err(WorkspaceRefusal::Compaction)?;
            crate::members::growth(input.before, &candidate)
                .map_err(WorkspaceRefusal::Membership)?;
            if maker != *input.owner
                && let Some(before) = input.before
            {
                super::membership::admit_base_operations(
                    before,
                    &candidate,
                    input.workspace,
                    (input.now_ns, keeps_checkpoints(&input)),
                )
                .map_err(membership)?;
            }
            // The receiver projects what it owns onto the base it adopts, so a
            // base leaving out or changing its entries holds only until then.
            checked(&input, &candidate, true)?;
            Ok(AdmittedWorkspace {
                candidate,
                rebased: true,
            })
        }
        super::base::Lineage::Refounded => Err(WorkspaceRefusal::Refounded.into()),
    }
}

/// The workspace's governance as this replica first takes it in, with nothing
/// held to judge it against: whoever sends it, every membership operation it
/// holds proves its author, it names this workspace and an owner, and the
/// membership those operations replay to lists one of `input.anchors` (this
/// replica's own device, or the device it joined through) as a device of a
/// person holding a role, or among the workspace's holders by its signed
/// address. A document founded anywhere else names none of them, so it
/// changes nothing here.
fn first_member_list(
    input: &WorkspaceAdmission<'_>,
    changes: &[Vec<u8>],
) -> Result<AdmittedWorkspace, AdmissionFailure> {
    let (candidate, rebased) = match super::base::lineage(None, changes) {
        super::base::Lineage::Same { changes, .. } => (super::candidate(None, &changes)?, false),
        super::base::Lineage::Newer { base, changes } => {
            (super::candidate_on(*base, &changes)?, true)
        }
        super::base::Lineage::Refounded => return Err(WorkspaceRefusal::Refounded.into()),
    };
    let declared = uuid::Uuid::parse_str(input.workspace)
        .map_err(|_| WorkspaceRefusal::DeclaredWorkspaceMismatch)?;
    if super::declared_workspace_mismatches(&candidate, declared) {
        return Err(WorkspaceRefusal::DeclaredWorkspaceMismatch.into());
    }
    // It starts at the workspace's founding or at a base its owner or an administrator signed,
    // and nowhere else: whoever carried it cannot hand this replica a history of its own making.
    super::compaction::verified_start(&candidate, input.workspace)
        .map_err(WorkspaceRefusal::Compaction)?;
    crate::members::growth(None, &candidate).map_err(WorkspaceRefusal::Membership)?;
    let policy = super::policy_of(&candidate, input.workspace);
    if policy.owner().is_none() {
        return Err(WorkspaceRefusal::RulesMissing.into());
    }
    // Each anchor's own row, and only the certificate it holds: the rows are
    // read once, whole, as the replica takes them in.
    let listed = |device: &[u8; 32]| {
        let Some(automerge::ScalarValue::Bytes(bytes)) =
            super::membership::winner(&candidate, "peer_identities_v1", &hex::encode(device))
        else {
            return false;
        };
        tilo_failure::ignored!(
            "a device row that does not decode lists no device",
            crate::state::DeviceRow::certificate_of(&bytes).ok()
        )
        .is_some_and(|cert| {
            cert.device_public_key == *device
                && policy
                    .active_role(&hex::encode(cert.user_public_key))
                    .is_some()
        })
    };
    let holders = crate::state::keeping_devices_of(&candidate);
    let held_by = |device: &[u8; 32]| {
        holders
            .iter()
            .any(|hint| hint.host.device_public_key == *device)
    };
    if !input
        .anchors
        .iter()
        .any(|device| listed(device) || held_by(device))
    {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    }
    Ok(AdmittedWorkspace { candidate, rebased })
}

/// What a governance batch from a sender holding no role may change: the
/// device rows and display names an admission writes, the invitation it
/// redeems, the membership operations it signs, and the grant a key its person
/// made for an agent or a program is admitted under.
const OWN_ADMISSION_KEYS: [&str; 6] = [
    "peers",
    "peer_identities_v1",
    crate::state::invitation_receipts::RECEIPTS_KEY,
    crate::state::invitation_receipts::PARTS_KEY,
    crate::members::MEMBERS_KEY,
    collab_types::workspace_grant::WORKSPACE_GRANTS_KEY,
];

/// What a sender holding no role may write: its own admission, signed by its
/// own device and nothing else. That is the redemption of an invitation
/// naming its person (`Admit { person }` of the author's own person, the
/// invitation its authority), or the device row of a further device of a
/// person the list names, written by that device (`Governance { rows:
/// [Identities] }`, admissible because its certificate chains to a root the
/// list names). Each added operation is authored by the sending device under
/// one certificate, and the only device row and name it writes are its own,
/// the row holding that certificate; every other part of the workspace stays
/// as held. [`super::membership::admit_operations`] then judges the
/// operations as for any writer. A holder that is no member carries what
/// members signed, each frame under its author, and writes nothing of its
/// own: its own copy's row, an admission of someone else or any record is
/// refused.
fn enrolling_writes(
    input: &WorkspaceAdmission<'_>,
    changes: &[Vec<u8>],
) -> Result<AdmittedWorkspace, AdmissionFailure> {
    let before = input.before.ok_or(WorkspaceRefusal::RulesMissing)?;
    let mut sender = [0; 32];
    hex::decode_to_slice(input.source.key, &mut sender)
        .map_err(|_| WorkspaceRefusal::WriteForbidden)?;
    let super::base::Lineage::Same { changes, replaced } =
        super::base::lineage(Some(before), changes)
    else {
        return Err(WorkspaceRefusal::GovernanceWriter.into());
    };
    let pruned = super::base::pruned(Some(before), replaced);
    let candidate = super::candidate(Some(pruned.as_ref().unwrap_or(before)), &changes)?;
    if !super::only_keys_differ(Some(before), &candidate, &OWN_ADMISSION_KEYS) {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    }
    // Read only where the batch wrote, never the workspace's every row: a
    // device listing itself costs what it wrote however many are listed.
    let written: Vec<automerge::Change> = changes
        .iter()
        .filter_map(|bytes| automerge::Change::try_from(bytes.as_slice()).ok())
        .collect();
    let touched =
        super::rows::touched(&candidate, &written).ok_or(WorkspaceRefusal::WriteForbidden)?;
    // A key its person made, listed already, records the grant it is admitted under beside
    // the row it wrote: that grant alone, and only its own.
    if !touched.is_empty()
        && touched
            .keys()
            .all(|key| key == collab_types::workspace_grant::WORKSPACE_GRANTS_KEY)
    {
        let author = listed(before, input.workspace, sender)?;
        own_grant((before, &candidate), &touched, &author)?;
        checked(input, &candidate, true)?;
        return Ok(AdmittedWorkspace {
            candidate,
            rebased: false,
        });
    }
    let author = own_operations((before, &candidate), &touched, sender)?;
    only_own_rows((before, &candidate), &touched, sender, &author)?;
    let grown =
        crate::members::growth(Some(before), &candidate).map_err(WorkspaceRefusal::Membership)?;
    if grown == 0 {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    }
    super::membership::admit_operations(
        Some(before),
        &candidate,
        &changes,
        input.workspace,
        (input.now_ns, false),
    )
    .map_err(membership)?;
    checked(input, &candidate, true)?;
    Ok(AdmittedWorkspace {
        candidate,
        rebased: false,
    })
}

/// The membership operations `candidate` adds to `before` where the batch
/// touched it, all `sender`'s own: its admission of its own person, or its own
/// device row. Answers their author.
fn own_operations(
    (before, candidate): (&AutoCommit, &AutoCommit),
    touched: &std::collections::BTreeMap<String, super::rows::Wrote>,
    sender: [u8; 32],
) -> Result<tilo_user_identity::DeviceCert, AdmissionFailure> {
    let added: Vec<_> = touched
        .keys()
        .filter(|key| crate::members::op::keyed(key).is_some())
        .filter(|key| super::membership::values(before, &[], key).is_empty())
        .flat_map(|key| super::membership::values(candidate, &[], key))
        .filter_map(|bytes| {
            tilo_failure::ignored!(
                "an operation that does not prove its author changes nothing",
                crate::members::SignedMembershipOp::verified_stored(&bytes).ok()
            )
            .map(|(_, op)| op)
        })
        .collect();
    let Some(first) = added.first() else {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    };
    let author = first.author.clone();
    let own = added.iter().all(|op| {
        op.author == author
            && op.author.device_public_key == sender
            && match &op.command {
                crate::members::MembershipOpCommand::Admit { person, .. }
                | crate::members::MembershipOpCommand::Enter { person, .. } => {
                    *person == op.author.user_public_key
                }
                crate::members::MembershipOpCommand::Governance { rows } => {
                    *rows == [super::GovernanceRow::Identities]
                }
                crate::members::MembershipOpCommand::Project { .. }
                | crate::members::MembershipOpCommand::Compact { .. }
                | crate::members::MembershipOpCommand::Readmit { .. } => false,
            }
    });
    if !own {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    }
    Ok(author)
}

/// The certificate `before` lists `sender`'s device under, its person holding a role in
/// `workspace`.
fn listed(
    before: &AutoCommit,
    workspace: &str,
    sender: [u8; 32],
) -> Result<tilo_user_identity::DeviceCert, AdmissionFailure> {
    let Some(automerge::ScalarValue::Bytes(bytes)) =
        super::membership::winner(before, "peer_identities_v1", &hex::encode(sender))
    else {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    };
    let cert = crate::state::DeviceRow::certificate_of(&bytes)
        .map_err(|_| WorkspaceRefusal::WriteForbidden)?;
    let policy = super::policy_of(before, workspace);
    if cert.device_public_key != sender
        || policy
            .active_role(&hex::encode(cert.user_public_key))
            .is_none()
    {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    }
    Ok(cert)
}

/// Whether the rows the batch changed are `sender`'s own: the only device row
/// it writes holds the certificate its operations are signed under, and the
/// names it writes are its own person's, and the only grant it records is the
/// one its own certificate is admitted under.
fn only_own_rows(
    (before, candidate): (&AutoCommit, &AutoCommit),
    touched: &std::collections::BTreeMap<String, super::rows::Wrote>,
    sender: [u8; 32],
    author: &tilo_user_identity::DeviceCert,
) -> Result<(), AdmissionFailure> {
    let own_key = hex::encode(sender);
    let person_key = hex::encode(author.user_public_key);
    let identity_of =
        |key: &str| match super::membership::winner(candidate, "peer_identities_v1", key) {
            Some(automerge::ScalarValue::Bytes(bytes)) => tilo_failure::ignored!(
                "a device row that does not decode is no device's own",
                crate::state::DeviceRow::decode(&bytes).ok()
            ),
            _ => None,
        };
    // The entries the batch changed of one of the maps an admission writes; a
    // map rewritten whole is no device's own row.
    let entries = |key: &str| -> Result<Vec<String>, AdmissionFailure> {
        match touched.get(key) {
            None => Ok(Vec::new()),
            Some(super::rows::Wrote::Entries(entries)) => Ok(entries
                .iter()
                .filter(|entry| {
                    super::membership::winner(before, key, entry)
                        != super::membership::winner(candidate, key, entry)
                })
                .cloned()
                .collect()),
            Some(super::rows::Wrote::Whole) => Err(WorkspaceRefusal::WriteForbidden.into()),
        }
    };
    // The only device row it writes is its own, holding the certificate its
    // operations are signed under; it takes no row away.
    let rows = entries("peer_identities_v1")?.into_iter().any(|device| {
        device != own_key
            || identity_of(&device).is_none_or(|identity| identity.device_cert != *author)
    });
    // A person's name is kept beside each device of theirs: naming
    // themselves, a device names every device of its own person, and none of
    // anyone else's.
    let names = entries("peers")?.into_iter().any(|key| {
        key != own_key
            && key != person_key
            && identity_of(&key).is_none_or(|identity| {
                identity.device_cert.user_public_key != author.user_public_key
            })
    });
    if rows || names {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    }
    own_grant((before, candidate), touched, author)
}

/// Whether the grants the batch wrote are only `author`'s own: the entry its
/// key is listed under, naming that key and its person under the scope its
/// certificate grants, unrevoked, and written over no revocation. The first
/// grant a workspace records makes the map they are kept in.
fn own_grant(
    (before, candidate): (&AutoCommit, &AutoCommit),
    touched: &std::collections::BTreeMap<String, super::rows::Wrote>,
    author: &tilo_user_identity::DeviceCert,
) -> Result<(), AdmissionFailure> {
    use automerge::ReadDoc as _;
    use collab_types::workspace_grant::{WORKSPACE_GRANTS_KEY, WorkspaceGrant};
    let grants = |doc: &AutoCommit| match doc.get(automerge::ROOT, WORKSPACE_GRANTS_KEY) {
        Ok(Some((automerge::Value::Object(automerge::ObjType::Map), grants))) => Some(grants),
        _ => None,
    };
    let written: Vec<String> = match touched.get(WORKSPACE_GRANTS_KEY) {
        None => return Ok(()),
        Some(super::rows::Wrote::Entries(entries)) => entries.iter().cloned().collect(),
        Some(super::rows::Wrote::Whole) => match (grants(before), grants(candidate)) {
            (None, Some(made)) => candidate.keys(&made).collect(),
            _ => return Err(WorkspaceRefusal::WriteForbidden.into()),
        },
    };
    let read = |doc: &AutoCommit, entry: &str| match super::membership::winner(
        doc,
        WORKSPACE_GRANTS_KEY,
        entry,
    ) {
        Some(automerge::ScalarValue::Str(json)) => tilo_failure::ignored!(
            "a grant that does not decode is no key's own",
            serde_json::from_str::<WorkspaceGrant>(&json).ok()
        ),
        _ => None,
    };
    let id = WorkspaceGrant::id_of(&author.device_public_key);
    let Some(scope) = author
        .capabilities
        .agent
        .as_ref()
        .map(tilo_user_identity::AgentScope::workspace_name)
    else {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    };
    let own = written.iter().all(|entry| {
        *entry == id
            && read(candidate, entry).is_some_and(|grant| {
                grant.id == id
                    && grant.device == hex::encode(author.device_public_key)
                    && grant.person == hex::encode(author.user_public_key)
                    && grant.scope == scope
                    && grant.revoked_ms.is_none()
            })
            && super::membership::winner(before, WORKSPACE_GRANTS_KEY, entry)
                .is_none_or(|_| read(before, entry).is_some_and(|grant| grant.revoked_ms.is_none()))
    });
    if !own {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    }
    Ok(())
}

fn membership(refusal: super::membership::MembershipRefusal) -> AdmissionFailure {
    match refusal {
        super::membership::MembershipRefusal::Device(reason) => AdmissionFailure::Device(reason),
        super::membership::MembershipRefusal::NotMembership => {
            WorkspaceRefusal::GovernanceWriter.into()
        }
        refusal => WorkspaceRefusal::Membership(refusal.to_string()).into(),
    }
}

/// Check a candidate workspace document, however it was built: from
/// `input.before` and a batch ([`admit_workspace_changes`]), or as a whole
/// checkpoint replacing it.
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
///    ([`crate::coverage::monotonic`], [`crate::durability::monotonic`]), and
/// 6. A shortcut names as who saved it only a proven saver
///    ([`super::saved_shortcut`]).
///
/// # Errors
///
/// The first [`WorkspaceRefusal`] that applies.
fn checked(
    input: &WorkspaceAdmission<'_>,
    candidate: &AutoCommit,
    hosted: bool,
) -> Result<(), WorkspaceRefusal> {
    let WorkspaceAdmission {
        before,
        workspace,
        source,
        owner,
        administrators,
        now_ns,
        ..
    } = *input;
    let keeps = source.keeps_history(owner, administrators);
    let declared = uuid::Uuid::parse_str(workspace)
        .map_err(|_| WorkspaceRefusal::DeclaredWorkspaceMismatch)?;
    if super::declared_workspace_mismatches(candidate, declared) {
        return Err(WorkspaceRefusal::DeclaredWorkspaceMismatch);
    }
    if !hosted && !super::governance_unchanged(before, candidate) {
        return Err(WorkspaceRefusal::GovernanceWriter);
    }
    if !keeps {
        // The owner's and the administrators' devices relay what members
        // recorded; anyone else records only their own.
        if !super::effect_slices_kept(before, candidate, source.person()) {
            return Err(WorkspaceRefusal::ReceiptWriter);
        }
        // A device's verdicts on other devices' writes are its own to keep
        // and clear, as its receipts are.
        let sender = tilo_failure::ignored!(
            "a key that is not a device key keeps no slice of its own, so every refusal it writes \
             is refused rather than trusted",
            hex::decode(source.key)
                .ok()
                .and_then(|key| <[u8; 32]>::try_from(key.as_slice()).ok())
        );
        if !super::refusal_slices_kept(before, candidate, source.person(), sender.as_ref()) {
            return Err(WorkspaceRefusal::RefusalWriter);
        }
        if !super::checkpoint_check_slices_kept(before, candidate, source.person(), sender.as_ref())
        {
            return Err(WorkspaceRefusal::CheckpointCheckWriter);
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
    crate::removals::monotonic(before, candidate).map_err(WorkspaceRefusal::Removals)?;
    crate::standing::monotonic(before, candidate).map_err(WorkspaceRefusal::Removals)?;
    crate::durability::monotonic(before, candidate).map_err(WorkspaceRefusal::Durability)
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
        .filter_map(|saved| {
            tilo_failure::ignored!(
                "a shortcut that does not decode proves no saver, so admits nothing",
                saved.decode().ok()
            )
        })
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

/// Whether a sender may make undoable changes on the workspace (`true`), or only read it and so record its own
/// effect receipts and give its own shares (`false`), decided against the
/// policy already held — never the one the batch brings.
/// Whether the sender's role holds the workspace's settings, so it keeps the
/// workspace's checkpoints (coverage, durability receipts and import floors).
/// Those move with every checkpoint a keeper of the workspace publishes, so
/// they are written as it publishes, with no operation recorded for each;
/// what they say is judged as for every writer: coverage and durability never
/// rewind ([`checked`]).
fn keeps_checkpoints(input: &WorkspaceAdmission<'_>) -> bool {
    let (Some(before), SenderAccess::Member { person, foreign }) =
        (input.before, input.source.access)
    else {
        return false;
    };
    let policy = crate::workspace::policy_of(before, input.workspace);
    let person = hex::encode(person);
    let authority = policy.person_on_device(&person, foreign);
    policy
        .check_action(
            &authority,
            tilo_action_policy::roles::governance::SETTINGS,
            &Context::at_secs(input.now_ns / 1_000_000_000),
            false,
        )
        .is_ok()
}

fn may_write(
    before: Option<&AutoCommit>,
    workspace: &str,
    source: &Writer<'_>,
    now_ns: u64,
) -> Result<bool, WorkspaceRefusal> {
    let (person, foreign) = match source.access {
        SenderAccess::Member { person, foreign } => (person, foreign),
        SenderAccess::TransportAuthorized => return Ok(true),
        SenderAccess::Enrolling => return Ok(false),
    };
    let before = before.ok_or(WorkspaceRefusal::RulesMissing)?;
    let policy = crate::workspace::policy_of(before, workspace);
    let person = hex::encode(person);
    if policy.active_role(&person).is_none() {
        return Err(WorkspaceRefusal::WriteForbidden);
    }
    let authority = policy.person_on_device(&person, foreign);
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
