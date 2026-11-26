//! Workspace-policy leaves shared by the shell and certified browser peer.
mod admit;
mod adopt;
pub mod base;
pub mod basis;
pub mod compaction;
pub mod membership;
pub(crate) mod policy;
pub(crate) mod read;
pub mod rows;
pub mod saved_shortcut;
pub mod share;
pub use admit::{
    AdmissionFailure, AdmittedWorkspace, SenderAccess, WorkspaceAdmission, WorkspaceRefusal,
    Writer, admit_workspace_changes,
};
pub use adopt::adopt_workspace;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
pub use rows::{GovernanceRow, changed_rows, changed_rows_by, governance_rows};
pub use saved_shortcut::{
    Saver, proven, proven_saver, sign_saved_shortcut, sign_saved_shortcut_as_agent,
    workspace_revocations,
};
use std::collections::BTreeMap;
use tilo_automerge::ReadTyped as _;
/// Every value the workspace's governance records, exactly as stored.
///
/// Member, grant and identity rows are the raw blobs the document holds, not their
/// decoded form: equality is the byte-exact comparison
/// [`governance_unchanged`] needs, so a device that may not write governance cannot
/// slip a changed but undecodable (or re-encoded) row past it. Decoded views of the
/// same document state live on its cache (`crate::cached_document::CachedDocument::policy`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceGovernance {
    /// The structural owner's person key (`owner`).
    pub owner: Option<String>,
    /// `sandbox.*`: JSON sandbox `CapOverride` blobs keyed by id.
    pub sandbox: BTreeMap<String, Vec<u8>>,
    /// `peers.*`: self-asserted display names keyed by device key, or by a
    /// person's user root for someone the workspace holds by root alone.
    pub peers: BTreeMap<String, String>,
    /// `peer_identities_v1.*`: postcard `DeviceRow` rows keyed by device key.
    pub identities: BTreeMap<String, Vec<u8>>,
    /// The workspace's checkpoint coverage bytes.
    pub coverage: Option<Vec<u8>>,
    /// `invitation_receipts_v1.*`: every redemption receipt, keyed by invite id.
    pub invitation_receipts: BTreeMap<String, Vec<Vec<u8>>>,
    /// `invitation_parts_v1.*`: the runs of signed invitations receipts
    /// share, keyed by their digest ([`crate::state::invitation_receipts`]).
    pub invitation_parts: BTreeMap<String, Vec<Vec<u8>>>,
}
impl WorkspaceGovernance {
    fn absent() -> Self {
        Self {
            owner: None,
            sandbox: BTreeMap::new(),
            peers: BTreeMap::new(),
            identities: BTreeMap::new(),
            coverage: None,
            invitation_receipts: BTreeMap::new(),
            invitation_parts: BTreeMap::new(),
        }
    }
}
#[cfg(test)]
thread_local! { pub(crate) static POLICY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
/// The workspace's authority: its membership as the operations its document
/// holds make it ([`crate::members::replay`]).
#[must_use]
pub fn policy_of(doc: &AutoCommit, workspace: &str) -> tilo_action_policy::WorkspacePolicy {
    crate::members::replay(doc, workspace)
}
pub fn governance_of(doc: Option<&AutoCommit>) -> WorkspaceGovernance {
    let Some(doc) = doc else {
        return WorkspaceGovernance::absent();
    };
    let mut governance = WorkspaceGovernance::absent();
    let result = read::visit_governance_entries(doc, |entry| {
        match entry {
            read::GovernanceEntry::Owner(value) => governance.owner = Some(value.to_owned()),
            read::GovernanceEntry::Sandbox { key, bytes } => {
                governance.sandbox.insert(key.to_owned(), bytes.to_vec());
            }
            read::GovernanceEntry::Peer { key, name } => {
                governance.peers.insert(key.to_owned(), name.to_owned());
            }
            read::GovernanceEntry::Identity { key, bytes } => {
                governance.identities.insert(key.to_owned(), bytes.to_vec());
            }
        }
        Ok::<(), std::convert::Infallible>(())
    });
    // The visitor is infallible.
    debug_assert!(result.is_ok());
    governance.coverage = crate::coverage::coverage_bytes(doc);
    governance.invitation_receipts = invitation_receipts(doc).unwrap_or_default();
    governance.invitation_parts = crate::state::invitation_receipts::parts(doc).unwrap_or_default();
    governance
}

/// The certificate and revocations one [`WorkspaceGovernance::identities`] row
/// carries, for a reader outside admission that judges the identity history
/// (a server's historical retention proof) without the rest of the row.
#[derive(Clone, Debug)]
pub struct IdentityRow {
    /// The device certificate the row names.
    pub device_cert: tilo_user_identity::DeviceCert,
    pub revocations: Vec<tilo_user_identity::RevocationEntry>,
}

/// Decode one [`WorkspaceGovernance::identities`] row exactly as admission does.
pub fn identity_row(bytes: &[u8]) -> Result<IdentityRow, String> {
    let identity = crate::state::DeviceRow::decode(bytes).map_err(|error| error.to_string())?;
    Ok(IdentityRow {
        device_cert: identity.device_cert,
        revocations: identity.revocations,
    })
}

/// The receipt log the workspace is founded with, when it holds one.
fn receipt_log(doc: &AutoCommit) -> Result<Option<automerge::ObjId>, String> {
    match doc
        .get(ROOT, crate::state::invitation_receipts::RECEIPTS_KEY)
        .map_err(|error| error.to_string())?
    {
        Some((Value::Object(automerge::ObjType::Map), log)) => Ok(Some(log)),
        Some(_) => Err("the governance's invitation receipt log is not a map".to_string()),
        None => Ok(None),
    }
}

/// The redemption receipts `doc` records, by invite id: every receipt it has
/// seen under one invitation, as the log holds them.
/// The receipts kept under `key`, one invitation's id: [`invitation_receipts`]
/// of one invitation, without reading anyone else's.
pub(crate) fn invitation_receipts_of(doc: &AutoCommit, key: &str) -> Result<Vec<Vec<u8>>, String> {
    let Some(log) = receipt_log(doc)? else {
        return Ok(Vec::new());
    };
    let mut receipts = Vec::new();
    for (value, _) in doc.get_all(&log, key).map_err(|error| error.to_string())? {
        match value {
            Value::Scalar(value) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => receipts.push(bytes.clone()),
                _ => return Err("the governance's invitation receipt is not bytes".to_string()),
            },
            Value::Object(_) => {
                return Err("the governance's invitation receipt is not bytes".to_string());
            }
        }
    }
    Ok(receipts)
}
pub(crate) fn invitation_receipts(
    doc: &AutoCommit,
) -> Result<BTreeMap<String, Vec<Vec<u8>>>, String> {
    let Some(log) = receipt_log(doc)? else {
        return Ok(BTreeMap::new());
    };
    let mut receipts: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    for key in doc.keys(&log) {
        let values = doc
            .get_all(&log, &key)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|(value, _)| match value {
                Value::Scalar(value) => match value.as_ref() {
                    ScalarValue::Bytes(bytes) => Ok(bytes.clone()),
                    _ => Err("the governance's invitation receipt is not bytes".to_string()),
                },
                Value::Object(_) => {
                    Err("the governance's invitation receipt is not bytes".to_string())
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        receipts.entry(key).or_default().extend(values);
    }
    Ok(receipts)
}

/// Whether every redemption `before` records is still in `after`, byte for
/// byte, under the invitation it redeemed, and every invitation part it
/// keeps is still kept: a receipt is checked on the invitation its parts
/// put back together.
///
/// An invitation admits one person: whoever redeems it writes its receipt, and
/// that receipt is the workspace's record from then on. A batch may redeem an
/// invitation the workspace holds no redemption of, and nothing else.
///
/// A log this build cannot read is not evidence of either: it keeps nothing.
pub(crate) fn redemptions_kept(before: Option<&AutoCommit>, after: &AutoCommit) -> bool {
    use crate::state::invitation_receipts::parts;
    let Some(before) = before else {
        return true;
    };
    let (Ok(held_parts), Ok(written_parts)) = (parts(before), parts(after)) else {
        return false;
    };
    let parts_kept = held_parts.iter().all(|(key, held)| {
        written_parts
            .get(key)
            .is_some_and(|written| held.iter().all(|value| written.contains(value)))
    });
    let (Ok(before), Ok(after)) = (invitation_receipts(before), invitation_receipts(after)) else {
        return false;
    };
    // An invitation the workspace redeemed keeps exactly its receipts; one it
    // never redeemed may be redeemed now.
    parts_kept
        && before.iter().all(|(invite, held)| {
            after.get(invite).is_some_and(|written| {
                held.iter().all(|value| written.contains(value))
                    && written.iter().all(|value| held.contains(value))
            })
        })
}

/// The workspace's name and authority as `candidate` records them, for the workspace
/// it declares. Rows that do not decode are skipped.
pub fn extract_listed_fields(
    candidate: &AutoCommit,
) -> (Option<String>, tilo_action_policy::WorkspacePolicy) {
    #[cfg(test)]
    POLICY_READS.with(|count| count.set(count.get().saturating_add(1)));
    let text = |key: &str| candidate.str_at(&ROOT, key);
    let workspace = text("workspace").unwrap_or_default();
    (
        text("name"),
        crate::workspace::policy_of(candidate, &workspace),
    )
}
/// Parse the complete batch and apply to a disposable candidate, never the owner.
/// Changes with absent dependencies remain queued, as with `apply_changes`.
/// A workspace document's change as the workspace sends it: compressed, once it
/// is large enough to gain from it. Governance holds certificates and receipts
/// as JSON text, which compresses several times over, and a workspace's base
/// carries all of it in one change: sent raw, a newcomer to a workspace of
/// hundreds downloaded megabytes before it saw anything. Every reader takes a
/// compressed change as it takes a raw one, and its hash is the raw change's.
#[must_use]
pub fn sent_change(mut change: automerge::Change) -> Vec<u8> {
    change.bytes().into_owned()
}

pub fn candidate(
    before: Option<&AutoCommit>,
    changes: &[Vec<u8>],
) -> Result<AutoCommit, AdmissionFailure> {
    candidate_on(before.cloned().unwrap_or_else(AutoCommit::new), changes)
}

/// [`candidate`] built on `candidate`, taken rather than copied.
///
/// # Errors
/// As [`candidate`].
pub fn candidate_on(
    mut candidate: AutoCommit,
    changes: &[Vec<u8>],
) -> Result<AutoCommit, AdmissionFailure> {
    // Each entry is one complete change chunk; their concatenation is ordinary
    // storage, admitted like any other untrusted document mutation.
    let storage = changes.concat();
    candidate.reset_diff_cursor();
    match apply_storage(&mut candidate, &storage) {
        Ok(()) => Ok(candidate),
        // A change under a number its writer gave another change the document holds is a second
        // branch of that writer, which Automerge refuses with the whole batch. Refusing the
        // batch refuses it again with every batch after, which carries it until the document
        // holds it: no change from that sender would land again. That change, and what builds
        // on it, is left out, and the rest is taken in; a refused apply leaves `candidate` as it
        // was.
        Err(refused) => match without_second_branches(&candidate, changes) {
            Some(kept) if kept.len() < changes.len() => {
                apply_storage(&mut candidate, &kept.concat())?;
                Ok(candidate)
            }
            _ => Err(refused),
        },
    }
}

/// Apply `storage` to `candidate`, keeping changes whose dependencies are not there yet.
fn apply_storage(candidate: &mut AutoCommit, storage: &[u8]) -> Result<(), AdmissionFailure> {
    if storage.is_empty() {
        return Ok(());
    }
    tilo_automerge_isolate::apply(
        candidate,
        storage,
        tilo_automerge_isolate::Dependencies::RetainOrphans,
    )
    .map(drop)
    .map_err(|error| match crate::AdmissionError::from(error) {
        crate::AdmissionError::Rejected { detail, .. } => {
            AdmissionFailure::Refused(WorkspaceRefusal::Malformed(detail))
        }
        crate::AdmissionError::Device(reason) => AdmissionFailure::Device(reason),
        other => AdmissionFailure::Refused(WorkspaceRefusal::Malformed(other.to_string())),
    })
}

/// `changes` without each that takes a number its writer gave another change `candidate` holds
/// or the batch carries before it, and without what builds on one left out. `None` when a
/// change does not read: the batch is decided as it came.
fn without_second_branches(candidate: &AutoCommit, changes: &[Vec<u8>]) -> Option<Vec<Vec<u8>>> {
    use std::collections::{HashMap, HashSet};
    let incoming = changes
        .iter()
        .map(|bytes| automerge::Change::try_from(bytes.as_slice()).ok())
        .collect::<Option<Vec<_>>>()?;
    let mut numbered: HashMap<(automerge::ActorId, u64), automerge::ChangeHash> = candidate
        .clone()
        .get_changes(&[])
        .into_iter()
        .map(|change| ((change.actor_id().clone(), change.seq()), change.hash()))
        .collect();
    let mut left_out = HashSet::new();
    let mut kept = Vec::with_capacity(changes.len());
    for (bytes, change) in changes.iter().zip(incoming) {
        let number = (change.actor_id().clone(), change.seq());
        let second = numbered
            .get(&number)
            .is_some_and(|other| *other != change.hash());
        if second || change.deps().iter().any(|dep| left_out.contains(dep)) {
            tracing::warn!(
                writer = %change.actor_id(),
                seq = change.seq(),
                "a workspace change under a number its writer gave another is left out"
            );
            left_out.insert(change.hash());
            continue;
        }
        numbered.insert(number, change.hash());
        kept.push(bytes.clone());
    }
    Some(kept)
}
pub fn declared_workspace_mismatches(candidate: &AutoCommit, workspace: uuid::Uuid) -> bool {
    let declared = match candidate.get(ROOT, "workspace").ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(value) => tilo_failure::ignored!(
                "a workspace id that does not parse declares no other workspace",
                uuid::Uuid::parse_str(value).ok()
            ),
            _ => None,
        },
        _ => None,
    };
    declared.is_some_and(|declared| declared != workspace)
}
/// Whether `after` carries the governance `before` does, compared by value: a
/// peer echoing governance it learned re-states it under its own actor, which
/// is not a change. Durable receipts and import proposals are in the governance too.
pub fn governance_unchanged(before: Option<&AutoCommit>, after: &AutoCommit) -> bool {
    changed_rows(before, Some(after), true).is_empty()
}

/// What `value` holds, as stored: bytes as bytes, and any value of another
/// shape as what it prints, so a writer's value of an unexpected shape is a
/// value too.
fn held_bytes(value: &Value<'_>) -> Vec<u8> {
    match value {
        Value::Scalar(value) => match value.as_ref() {
            ScalarValue::Bytes(bytes) => bytes.clone(),
            other => format!("{other:?}").into_bytes(),
        },
        Value::Object(kind) => format!("{kind:?}").into_bytes(),
    }
}

/// The workspace's settings as stored: its name, its saved grant sets and its
/// entry.
/// Whatever a key holds counts, so a value of another shape than a writer
/// writes is a change too.
fn settings_of(doc: Option<&AutoCommit>) -> Vec<(String, Vec<u8>)> {
    use tilo_desktop_model::projection::{
        WORKSPACE_CONTENT_KEY, WORKSPACE_ENTRY_KEY, WORKSPACE_NAME_KEY, WORKSPACE_PRESETS_KEY,
    };
    let Some(doc) = doc else {
        return Vec::new();
    };
    let mut settings = Vec::new();
    for key in [
        WORKSPACE_NAME_KEY,
        WORKSPACE_PRESETS_KEY,
        WORKSPACE_ENTRY_KEY,
        WORKSPACE_CONTENT_KEY,
    ] {
        let Some((value, obj)) = doc.get(ROOT, key).ok().flatten() else {
            continue;
        };
        settings.push((key.to_owned(), held_bytes(&value)));
        if matches!(value, Value::Object(_)) {
            for entry in doc.keys(&obj) {
                if let Some((value, _)) = doc.get(&obj, &entry).ok().flatten() {
                    settings.push((format!("{key}/{entry}"), held_bytes(&value)));
                }
            }
        }
    }
    settings
}
/// The key holding effect receipts: `{<person>/<invocation> → JSON receipt}}`.
const EFFECT_RECEIPTS: &str = tilo_desktop_model::projection::WORKSPACE_EFFECT_RECEIPTS_KEY;
/// The key anchoring archived receipts: `{<person>/<device> → root}`.
const EFFECT_HISTORY: &str = tilo_desktop_model::projection::WORKSPACE_EFFECT_HISTORY_KEY;
/// The key holding write refusals: `{<person>/<device> → {route and author →
/// postcard refusal}}`.
const WRITE_REFUSALS: &str = tilo_desktop_model::projection::WORKSPACE_WRITE_REFUSALS_KEY;

/// Decode one stored effect receipt. An unreadable receipt carries no evidence:
/// it is treated as absent everywhere, as it always has been.
fn decode_receipt(bytes: &[u8]) -> Option<serde_json::Value> {
    tilo_failure::ignored!(
        "an unreadable receipt carries no evidence",
        serde_json::from_slice(bytes).ok()
    )
}

/// Every row of the founded map `key`, `{<row> → bytes}`, borrowed, with no
/// decoding.
fn visit_row_bytes<E>(
    doc: &AutoCommit,
    key: &str,
    mut visit: impl FnMut(&str, &[u8]) -> Result<(), E>,
) -> Result<(), E> {
    let Some((Value::Object(_), rows)) = doc.get(ROOT, key).ok().flatten() else {
        return Ok(());
    };
    for row in doc.keys(&rows) {
        if let Some((Value::Scalar(value), _)) = doc.get(&rows, row.as_str()).ok().flatten()
            && let ScalarValue::Bytes(bytes) = value.as_ref()
        {
            visit(&row, bytes)?;
        }
    }
    Ok(())
}

/// The invocation id a receipt's row key names.
fn receipt_invocation(row: &str) -> &str {
    row.split_once('/')
        .map_or(row, |(_, invocation)| invocation)
}

/// What `doc` holds under the founded map `key`, `{<row> → bytes}`, as stored,
/// with any value of another shape counted too: the rows a member writes.
fn row_values(doc: Option<&AutoCommit>, key: &str) -> BTreeMap<String, Vec<u8>> {
    let mut values = BTreeMap::new();
    let Some(doc) = doc else {
        return values;
    };
    let Some((Value::Object(_), rows)) = doc.get(ROOT, key).ok().flatten() else {
        return values;
    };
    for row in doc.keys(&rows) {
        if let Some((value, _)) = doc.get(&rows, row.as_str()).ok().flatten() {
            values.insert(row, held_bytes(&value));
        }
    }
    values
}

/// The person and rest of a row key `<person>/<rest>`.
fn row_parts(row: &str) -> Option<(&str, &str)> {
    row.split_once('/')
}

/// Whether every effect receipt and history anchor `after` adds or changes
/// against `before` is `person`'s own (none is when no person is known):
/// in their row, each new receipt
/// naming them or an agent acting for them as its actor, and each anchor in
/// their row. A receipt is its
/// recorder's: only that member's devices write their rows.
#[must_use]
pub fn effect_slices_kept(
    before: Option<&AutoCommit>,
    after: &AutoCommit,
    person: Option<&[u8; 32]>,
) -> bool {
    let person = person.map(hex::encode).unwrap_or_default();
    let own =
        |row: &str| !person.is_empty() && row_parts(row).is_some_and(|(slice, _)| slice == person);
    let held = row_values(before, EFFECT_RECEIPTS);
    let receipts = row_values(Some(after), EFFECT_RECEIPTS);
    for (row, bytes) in &receipts {
        if held.get(row) == Some(bytes) {
            continue;
        }
        if !own(row) {
            return false;
        }
        let Some((_, invocation)) = row_parts(row) else {
            continue;
        };
        let Ok(receipt) =
            serde_json::from_slice::<tilo_actions::receipt::WorkspaceEffectReceipt>(bytes)
        else {
            return false;
        };
        if receipt.invocation.0.to_string() != invocation
            || !(receipt.actor.person_id() == Some(person.as_str())
                || receipt.actor.agent_id().is_some()
                || receipt.actor.app().is_some())
        {
            return false;
        }
    }
    let anchors = row_values(before, EFFECT_HISTORY);
    let written = row_values(Some(after), EFFECT_HISTORY);
    anchors
        .keys()
        .chain(written.keys())
        .filter(|row| anchors.get(*row) != written.get(*row))
        .all(|row| own(row))
}

/// Whether every write refusal `after` adds, replaces or clears against
/// `before` is `person`'s own device `device`'s: in that device's own row of
/// the key, and each verdict it writes there signed by it. A refusal is its
/// recorder's verdict; no other device keeps, rewrites or clears it.
#[must_use]
pub fn refusal_slices_kept(
    before: Option<&AutoCommit>,
    after: &AutoCommit,
    person: Option<&[u8; 32]>,
    device: Option<&[u8; 32]>,
) -> bool {
    device_rows_kept(
        before,
        after,
        WRITE_REFUSALS,
        (person, device),
        &|bytes, device| {
            postcard::from_bytes::<neighbour_wire::WriteRefusal>(bytes)
                .is_ok_and(|refusal| refusal.by == *device && refusal.signed_by(device))
        },
    )
}

/// Whether every checkpoint check `after` adds, replaces or clears against `before` is `person`'s
/// own device `device`'s: in that device's own row of [`crate::CHECKPOINT_CHECKS_KEY`], and each
/// check it keeps there signed by it. A check is its device's word that a record's checkpoint
/// holds what the writes it stands in for merge to, and the writes are dropped once every device
/// with the record's keys gave it: no device gives it for another.
#[must_use]
pub fn checkpoint_check_slices_kept(
    before: Option<&AutoCommit>,
    after: &AutoCommit,
    person: Option<&[u8; 32]>,
    device: Option<&[u8; 32]>,
) -> bool {
    device_rows_kept(
        before,
        after,
        crate::CHECKPOINT_CHECKS_KEY,
        (person, device),
        &crate::state::checkpoint_check_kept_by,
    )
}

/// Whether every value `after` adds, replaces or clears against `before` under `key`, a map of
/// rows `{<person>/<device> → {<entry> → bytes}}` each device writes for itself, is in `person`'s
/// device `device`'s own row, each value kept there one `valid` says that device signed.
fn device_rows_kept(
    before: Option<&AutoCommit>,
    after: &AutoCommit,
    key: &str,
    (person, device): (Option<&[u8; 32]>, Option<&[u8; 32]>),
    valid: &dyn Fn(&[u8], &[u8; 32]) -> bool,
) -> bool {
    use automerge::patches::PatchAction;
    use automerge::{Prop, Value as Plain};
    let mut after = after.clone();
    let from = before
        .map(|doc| doc.clone().get_heads())
        .unwrap_or_default();
    let to = after.get_heads();
    for patch in after.diff(&from, &to) {
        let mut keys: Vec<String> = Vec::new();
        let mut nested = true;
        for (_, prop) in &patch.path {
            match prop {
                Prop::Map(key) => keys.push(key.clone()),
                Prop::Seq(_) => {
                    nested = false;
                    break;
                }
            }
        }
        let value = match &patch.action {
            PatchAction::PutMap { key, value, .. } => {
                keys.push(key.clone());
                Some(&value.0)
            }
            PatchAction::DeleteMap { key } => {
                keys.push(key.clone());
                None
            }
            PatchAction::Conflict { prop } | PatchAction::Increment { prop, .. } => match prop {
                Prop::Map(key) => {
                    keys.push(key.clone());
                    None
                }
                Prop::Seq(_) => {
                    nested = false;
                    None
                }
            },
            _ => {
                nested = false;
                None
            }
        };
        if keys.first().map(String::as_str) != Some(key) {
            // Some other key: other rules decide it. A change to this key's
            // own shape never leaves it, so `nested` false here means it was
            // never this key.
            continue;
        }
        if !nested {
            return false;
        }
        if keys.len() == 1 {
            // The map under the key: every refuser writes into it, no one
            // takes it away.
            if !matches!(value, Some(Plain::Object(_))) {
                return false;
            }
            continue;
        }
        let (Some(person), Some(device)) = (person, device) else {
            // A sender the receiver cannot name as a person files no verdict.
            return false;
        };
        if keys.get(1) != Some(&format!("{}/{}", hex::encode(person), hex::encode(device))) {
            // A row is the device's own, in its person's name.
            return false;
        }
        match keys.len() {
            // The device's own row.
            2 => {
                if !matches!(value, Some(Plain::Object(_))) {
                    return false;
                }
            }
            // One value, kept or cleared in the device's own row: a kept
            // one is that device's, over exactly its bytes.
            3 => {
                let Some(Plain::Scalar(value)) = value else {
                    return false;
                };
                let ScalarValue::Bytes(bytes) = value.as_ref() else {
                    return false;
                };
                if !valid(bytes, device) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// What one who only reads a workspace may still change in it: their own effect
/// receipts and history anchors ([`effect_slices_kept`]), their own write
/// refusals ([`refusal_slices_kept`]), their own checkpoint checks
/// ([`checkpoint_check_slices_kept`]), and the membership operations they hold
/// (the shares they give among them)
/// — an operation is its author's, and one who only reads hands on what others
/// signed without admitting it.
const READER_WRITES: [&str; 5] = [
    EFFECT_RECEIPTS,
    EFFECT_HISTORY,
    WRITE_REFUSALS,
    crate::CHECKPOINT_CHECKS_KEY,
    crate::members::MEMBERS_KEY,
];

/// Whether `after` differs from `before` only in what one who only reads
/// the workspace may change in it ([`READER_WRITES`]).
#[must_use]
pub fn only_reader_writes_differ(before: Option<&AutoCommit>, after: &AutoCommit) -> bool {
    only_keys_differ(before, after, &READER_WRITES)
}

/// Whether `after` differs from `before` only under the root keys `keys`
/// and the membership operations' own keys.
#[must_use]
pub fn only_keys_differ(before: Option<&AutoCommit>, after: &AutoCommit, keys: &[&str]) -> bool {
    use automerge::{Prop, patches::PatchAction};
    let mut after = after.clone();
    let from = before
        .map(|doc| doc.clone().get_heads())
        .unwrap_or_default();
    let to = after.get_heads();
    after.diff(&from, &to).iter().all(|patch| {
        let key = match patch.path.first() {
            Some((_, Prop::Map(key))) => Some(key.as_str()),
            Some((_, Prop::Seq(_))) => None,
            None => match &patch.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    Some(key.as_str())
                }
                PatchAction::Conflict {
                    prop: Prop::Map(key),
                }
                | PatchAction::Increment {
                    prop: Prop::Map(key),
                    ..
                } => Some(key.as_str()),
                _ => None,
            },
        };
        key.is_some_and(|key| keys.contains(&key) || crate::members::op::keyed(key).is_some())
    })
}

/// Whether `new` carries different evidence than `prior` for one invocation.
/// Equal bytes are the same receipt and are never decoded; only a differing
/// encoding is compared by value, so re-encoding the same JSON is no rewrite.
fn receipt_rewritten(prior: &[u8], new: &[u8]) -> bool {
    prior != new
        && match (decode_receipt(prior), decode_receipt(new)) {
            (Some(prior), Some(new)) => prior != new,
            // Unreadable on either side: no evidence to rewrite, as before.
            _ => false,
        }
}

/// Effect receipts are immutable by value; omission cannot erase old evidence.
///
/// Compares the stored bytes of each receipt `before` holds against the same
/// row in `after`; a receipt is decoded only when its bytes changed.
pub fn immutable_receipts(before: Option<&AutoCommit>, after: &AutoCommit) -> bool {
    let Some(before) = before else {
        return true;
    };
    let Some((_, log)) = after.get(ROOT, EFFECT_RECEIPTS).ok().flatten() else {
        return true;
    };
    visit_row_bytes(before, EFFECT_RECEIPTS, |row, prior| {
        let rewritten = after
            .get(&log, row)
            .ok()
            .flatten()
            .is_some_and(|(value, _)| {
                matches!(value, Value::Scalar(value)
                    if matches!(value.as_ref(), ScalarValue::Bytes(new) if receipt_rewritten(prior, new)))
            });
        if rewritten { Err(()) } else { Ok(()) }
    })
    .is_ok()
}

/// Record every effect receipt `doc` carries in `log`, the receipts this
/// peer has admitted for the workspace's lifetime. A receipt the log already holds
/// byte-for-byte costs one comparison; a new one is decoded once (an unreadable
/// receipt is not evidence and is not recorded) and stored as received, never
/// re-encoded.
///
/// # Errors
///
/// [`WorkspaceRefusal::ReceiptImmutable`] when `doc` rewrites a receipt the log
/// holds; `log` may then be partly updated and must be discarded.
pub(crate) fn admit_receipts(
    log: &mut BTreeMap<String, Vec<u8>>,
    doc: &AutoCommit,
) -> Result<(), WorkspaceRefusal> {
    visit_row_bytes(doc, EFFECT_RECEIPTS, |row, bytes| {
        let key = receipt_invocation(row);
        let Some(prior) = log.get(key) else {
            if decode_receipt(bytes).is_some() {
                log.insert(key.to_owned(), bytes.to_vec());
            }
            return Ok(());
        };
        if receipt_rewritten(prior, bytes) {
            return Err(WorkspaceRefusal::ReceiptImmutable);
        }
        Ok(())
    })
}

/// Every readable effect receipt in `doc`, decoded, by its invocation id.
pub fn receipt_values(doc: &AutoCommit) -> BTreeMap<String, serde_json::Value> {
    let mut values = BTreeMap::new();
    let _ = tilo_failure::ignored!(
        "the visitor's error type is `Infallible`, so an unreadable receipt drops out",
        visit_row_bytes(doc, EFFECT_RECEIPTS, |row, bytes| {
            if let Some(value) = decode_receipt(bytes) {
                values.insert(receipt_invocation(row).to_owned(), value);
            }
            Ok::<(), std::convert::Infallible>(())
        })
    );
    values
}
