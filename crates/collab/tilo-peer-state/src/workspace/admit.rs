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
    /// The sender is a device of `person`, who is no member, named among the
    /// workspace's holders by its own signed address: a server of another
    /// person keeping the workspace. It writes the redemptions of
    /// invitations it answers and its own copy's row, and nothing else
    /// ([`holder_writes`]).
    Holder { person: [u8; 32] },
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
            SenderAccess::Member { person, .. } | SenderAccess::Holder { person } => Some(person),
            SenderAccess::TransportAuthorized => None,
        }
    }

    /// Whether the sender is a device of `owner`: the workspace's governance is
    /// theirs to write, from any device their root certifies. The receiver
    /// knows the owner from what it joined through, never from the batch,
    /// so a first answer cannot make its sender the owner.
    fn governs(&self, owner: &[u8; 32]) -> bool {
        matches!(self.access, SenderAccess::Member { person, .. } if person == *owner)
    }

    /// Whether the sender keeps the workspace's history for its members: a
    /// device of its owner, or of one of `administrators`, from a device of
    /// their own. They relay what members recorded and send the workspace's
    /// compacted base; only the owner's devices write its governance beyond
    /// the operations every receiver judges ([`Self::governs`]).
    fn keeps_history(&self, owner: &[u8; 32], administrators: &[[u8; 32]]) -> bool {
        self.governs(owner)
            || matches!(
                self.access,
                SenderAccess::Member { person, foreign: false } if administrators.contains(&person)
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
    /// The sender is not the workspace's owner and nothing is held to check
    /// its membership against.
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
                f.write_str("governed invitation has conflicting redemption receipts")
            }
            Self::ReceiptWriter => f.write_str("effect receipt is not its writer's own"),
            Self::RefusalWriter => f.write_str("write refusal is not its recorder's own"),
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
/// 1. The sender may make undoable changes on the workspace, unless it is the
///    owner's device (who may grant itself anything) — [`SenderAccess`].
/// 2. The batch decodes under the shared incoming limits ([`super::candidate`]).
/// 3. Then [`check_candidate`].
///
/// # Errors
///
/// The first [`WorkspaceRefusal`] that applies, or [`AdmissionFailure::Device`]
/// when this device could not decide the batch; nothing is admitted then.
pub fn admit_workspace_changes(
    input: WorkspaceAdmission<'_>,
    changes: &[Vec<u8>],
) -> Result<AdmittedWorkspace, AdmissionFailure> {
    if matches!(input.source.access, SenderAccess::Holder { .. }) {
        return holder_writes(&input, changes);
    }
    let governs = input.source.governs(input.owner);
    let keeps = input
        .source
        .keeps_history(input.owner, input.administrators);
    let writes = governs || may_write(input.before, input.workspace, &input.source, input.now_ns)?;
    match super::base::lineage(input.before, changes) {
        super::base::Lineage::Same { changes, replaced } => {
            // Changes waiting in the document that the batch shows to be of
            // another history go before the batch is applied.
            let pruned = super::base::pruned(input.before, replaced);
            let candidate = super::candidate(pruned.as_ref().or(input.before), &changes)?;
            // One who only reads the workspace records only their own receipts
            // and gives only their own shares.
            if !writes && !super::only_reader_writes_differ(input.before, &candidate) {
                return Err(WorkspaceRefusal::WriteForbidden.into());
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
                // A change to the workspace's governance is an operation's: a
                // batch that changes it without one changes nothing.
                return Err(WorkspaceRefusal::GovernanceWriter.into());
            } else {
                false
            };
            checked(&input, &candidate, governed)?;
            Ok(AdmittedWorkspace {
                candidate,
                rebased: false,
            })
        }
        super::base::Lineage::Newer { base, changes } if keeps => {
            let candidate = super::candidate_on(*base, &changes)?;
            // The receiver projects what it owns onto the base it adopts, so a
            // base leaving out or changing its entries holds only until then.
            checked_as(&input, &candidate, false)?;
            Ok(AdmittedWorkspace {
                candidate,
                rebased: true,
            })
        }
        super::base::Lineage::Newer { .. } => Err(WorkspaceRefusal::GovernanceWriter.into()),
        super::base::Lineage::Refounded => Err(WorkspaceRefusal::Refounded.into()),
    }
}

/// What a holder that is no member may write: the row saying where its own
/// copy stands ([`crate::state::HELD_COPIES_KEY`]), and the redemptions of
/// invitations, each admitting the person its signed invitation names
/// ([`super::membership::admit_operations`]), and the device rows of a
/// member's further device or key, which any key holder writes in
/// ([`super::membership::admit_operations`] checks the row's person is a
/// member's); nothing else. Each is a rule every device computes alike: the
/// row is about the holder itself, and a redemption or device row carries
/// what decides it. Whether the sender is a holder is read where the batch
/// was written (its dependencies), from the signed holder entries there, so
/// every receiver decides it alike whenever the batch reaches it. Adding
/// people otherwise, every change of a role, windows and layout stay a
/// member's.
fn holder_writes(
    input: &WorkspaceAdmission<'_>,
    changes: &[Vec<u8>],
) -> Result<AdmittedWorkspace, AdmissionFailure> {
    let before = input.before.ok_or(WorkspaceRefusal::RulesMissing)?;
    let at = super::membership::dependency_state(before, changes).map_err(membership)?;
    let named = crate::state::keeping_devices_of(&at)
        .iter()
        .any(|hint| hex::encode(hint.host.device_public_key) == input.source.key);
    if !named {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    }
    let super::base::Lineage::Same { changes, replaced } =
        super::base::lineage(Some(before), changes)
    else {
        return Err(WorkspaceRefusal::GovernanceWriter.into());
    };
    let pruned = super::base::pruned(Some(before), replaced);
    let candidate = super::candidate(Some(pruned.as_ref().unwrap_or(before)), &changes)?;
    if only_own_copy_row(before, &candidate, input.source.key) {
        checked(input, &candidate, false)?;
        return Ok(AdmittedWorkspace {
            candidate,
            rebased: false,
        });
    }
    let held = crate::members::ops(before);
    let only_admissions = crate::members::ops(&candidate)
        .iter()
        .filter(|(id, _)| !held.contains_key(*id))
        .all(|(_, op)| match &op.command {
            crate::members::MembershipOpCommand::Admit { .. } => true,
            // A member's further device is a device row and nothing else.
            crate::members::MembershipOpCommand::Governance { rows } => {
                *rows == [super::GovernanceRow::Identities]
            }
            crate::members::MembershipOpCommand::Project { .. } => false,
        });
    let grown =
        crate::members::growth(Some(before), &candidate).map_err(WorkspaceRefusal::Membership)?;
    if !only_admissions || grown == 0 {
        return Err(WorkspaceRefusal::WriteForbidden.into());
    }
    super::membership::admit_operations(
        Some(before),
        &candidate,
        &changes,
        input.workspace,
        input.now_ns,
    )
    .map_err(membership)?;
    checked(input, &candidate, true)?;
    Ok(AdmittedWorkspace {
        candidate,
        rebased: false,
    })
}

/// Whether all `after` changes of `before` is the row of `own` (a device
/// key, hex) among the held copies, and the map holding the rows when
/// `before` had none.
fn only_own_copy_row(before: &AutoCommit, after: &AutoCommit, own: &str) -> bool {
    use automerge::patches::PatchAction;
    use automerge::{ObjType, Prop, ReadDoc as _, Value};
    let rows = crate::state::HELD_COPIES_KEY;
    let had_rows = before.get(automerge::ROOT, rows).ok().flatten().is_some();
    let from = before.clone().get_heads();
    let mut after = after.clone();
    let to = after.get_heads();
    let patches = after.diff(&from, &to);
    !patches.is_empty()
        && patches
            .iter()
            .all(|patch| match (patch.path.as_slice(), &patch.action) {
                (
                    [],
                    PatchAction::PutMap {
                        key,
                        value: (Value::Object(ObjType::Map), _),
                        conflict: false,
                    },
                ) => key == rows && !had_rows,
                (
                    [(_, Prop::Map(map))],
                    PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key },
                ) => map == rows && key == own,
                _ => false,
            })
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
fn checked_as(
    input: &WorkspaceAdmission<'_>,
    candidate: &AutoCommit,
    governed: bool,
) -> Result<(), WorkspaceRefusal> {
    let WorkspaceAdmission {
        before,
        workspace,
        source,
        owner,
        administrators,
        now_ns,
    } = *input;
    let governs = source.governs(owner);
    let keeps = source.keeps_history(owner, administrators);
    let declared = uuid::Uuid::parse_str(workspace)
        .map_err(|_| WorkspaceRefusal::DeclaredWorkspaceMismatch)?;
    if super::declared_workspace_mismatches(candidate, declared) {
        return Err(WorkspaceRefusal::DeclaredWorkspaceMismatch);
    }
    if !governs && !governed && !super::governance_unchanged(before, candidate) {
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

/// [`checked_as`] where the receiver carries nothing over: every batch but a
/// newer base taken in by a receiver that keeps its delegations.
fn checked(
    input: &WorkspaceAdmission<'_>,
    candidate: &AutoCommit,
    governed: bool,
) -> Result<(), WorkspaceRefusal> {
    checked_as(input, candidate, governed)
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
    let SenderAccess::Member { person, foreign } = source.access else {
        return Ok(true);
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
