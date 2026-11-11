//! Workspace-policy leaves shared by the shell and certified browser peer.
mod admit;
mod adopt;
pub mod base;
pub mod media;
pub mod membership;
pub(crate) mod policy;
pub(crate) mod private_policy;
pub(crate) mod read;
pub mod rows;
pub mod saved_shortcut;
pub mod share;
pub use admit::{
    AdmittedWorkspace, Standing, WorkspaceAdmission, WorkspaceRefusal, Writer,
    admit_workspace_changes,
};
pub use adopt::adopt_workspace;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
pub use rows::{GovernanceRow, changed_rows, changed_rows_by, governance_rows};
pub use saved_shortcut::{proven_saver, sign_saved_shortcut, workspace_revocations};
use std::collections::BTreeMap;
use tilo_automerge::ReadTyped as _;
/// Every governed value a workspace document records, exactly as stored.
///
/// Member, grant and identity rows are the raw blobs the document holds, not their
/// decoded form: equality is the byte-exact comparison
/// [`governance_unchanged`] needs, so a non-governor cannot slip a changed but
/// undecodable (or re-encoded) row past it. Decoded views of the same document
/// state live on its cache (`crate::cached_document::CachedDocument::policy`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceGovernance {
    /// The structural owner's person key (`owner`).
    pub owner: Option<String>,
    /// `members.*`: JSON role blobs keyed by person.
    pub members: BTreeMap<String, Vec<u8>>,
    /// `members_joined_v1.*`: when the workspace admitted each member, Unix
    /// milliseconds as JSON, keyed by person.
    pub members_joined: BTreeMap<String, Vec<u8>>,
    /// `grants.*`: JSON `Grant` blobs keyed by grant id.
    pub grants: BTreeMap<String, Vec<u8>>,
    /// `sandbox.*`: JSON sandbox `CapOverride` blobs keyed by id.
    pub sandbox: BTreeMap<String, Vec<u8>>,
    /// `peers.*`: self-asserted display names keyed by device key.
    pub peers: BTreeMap<String, String>,
    /// `peer_identities_v1.*`: JSON `GovernedIdentity` blobs keyed by device key.
    pub identities: BTreeMap<String, Vec<u8>>,
    /// The governed checkpoint coverage bytes.
    pub coverage: Option<Vec<u8>>,
    /// `invitation_receipts_v1.*`: every redemption receipt, keyed by invite id.
    pub invitation_receipts: BTreeMap<String, Vec<Vec<u8>>>,
    /// The governed media policy, by value.
    pub media: media::MediaGovernanceValue,
}
impl WorkspaceGovernance {
    fn absent() -> Self {
        Self {
            owner: None,
            members: BTreeMap::new(),
            members_joined: BTreeMap::new(),
            grants: BTreeMap::new(),
            sandbox: BTreeMap::new(),
            peers: BTreeMap::new(),
            identities: BTreeMap::new(),
            coverage: None,
            invitation_receipts: BTreeMap::new(),
            media: media::MediaGovernanceValue::Absent,
        }
    }
}
#[cfg(test)]
thread_local! { pub(crate) static POLICY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
/// The workspace's authority as `doc`'s rows record it
/// ([`tilo_desktop_model::projection::workspace_policy_of`]): a whole read of
/// its governance, counted as one ([`crate::governance_work`]).
#[must_use]
pub fn policy_of(doc: &AutoCommit, workspace: &str) -> tilo_action_policy::WorkspacePolicy {
    crate::governance_work::governance_read();
    tilo_desktop_model::projection::workspace_policy_of(doc, workspace)
}
pub fn governance_of(doc: Option<&AutoCommit>) -> WorkspaceGovernance {
    let Some(doc) = doc else {
        return WorkspaceGovernance::absent();
    };
    let mut governance = WorkspaceGovernance::absent();
    // The legacy public output is unqualified owned data. The same borrowed
    // traversal serves bounded destinations that pay for each output copy.
    let result = read::visit_governance_entries(&read::LegacyRead(doc), |entry| {
        match entry {
            read::GovernanceEntry::Owner(value) => governance.owner = Some(value.to_owned()),
            read::GovernanceEntry::Grant { key, bytes } => {
                governance.grants.insert(key.to_owned(), bytes.to_vec());
            }
            read::GovernanceEntry::Member { key, bytes } => {
                governance.members.insert(key.to_owned(), bytes.to_vec());
            }
            read::GovernanceEntry::MemberJoined { key, bytes } => {
                governance
                    .members_joined
                    .insert(key.to_owned(), bytes.to_vec());
            }
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
    // LegacyRead has no resource refusals and the visitor is infallible.
    debug_assert!(result.is_ok());
    governance.coverage = crate::coverage::governed_bytes(doc);
    governance.invitation_receipts = invitation_receipts(doc).unwrap_or_default();
    governance.media = media::governed_value(Some(doc));
    governance
}

/// The certificate and revocations one [`WorkspaceGovernance::identities`] row
/// carries, for a reader outside admission that judges the identity history
/// (a relay's historical retention proof) without the rest of the row.
#[derive(Clone, Debug)]
pub struct IdentityRow {
    /// The governed device certificate.
    pub device_cert: tilo_user_identity::DeviceCert,
    pub revocations: Vec<tilo_user_identity::RevocationEntry>,
}

/// Decode one [`WorkspaceGovernance::identities`] row exactly as admission does.
pub fn identity_row(bytes: &[u8]) -> Result<IdentityRow, serde_json::Error> {
    let identity = crate::state::GovernedIdentity::decode(bytes)?;
    Ok(IdentityRow {
        device_cert: identity.device_cert,
        revocations: identity.revocations,
    })
}

/// The redemption receipts `doc` records, by invite id: every receipt it has
/// seen under one invitation, as the ledger holds them.
/// The receipts kept under `key`, one invitation's id, in every receipt
/// ledger the document holds: [`invitation_receipts`] of one invitation,
/// without reading anyone else's.
pub(crate) fn invitation_receipts_of(doc: &AutoCommit, key: &str) -> Result<Vec<Vec<u8>>, String> {
    let mut receipts = Vec::new();
    for (value, ledger) in doc
        .get_all(ROOT, "invitation_receipts_v1")
        .map_err(|error| error.to_string())?
    {
        if !matches!(value, Value::Object(automerge::ObjType::Map)) {
            return Err("governed invitation receipt ledger is not a map".to_string());
        }
        for (value, _) in doc
            .get_all(&ledger, key)
            .map_err(|error| error.to_string())?
        {
            match value {
                Value::Scalar(value) => match value.as_ref() {
                    ScalarValue::Bytes(bytes) => receipts.push(bytes.clone()),
                    _ => return Err("governed invitation receipt is not bytes".to_string()),
                },
                Value::Object(_) => {
                    return Err("governed invitation receipt is not bytes".to_string());
                }
            }
        }
    }
    Ok(receipts)
}
pub(crate) fn invitation_receipts(
    doc: &AutoCommit,
) -> Result<BTreeMap<String, Vec<Vec<u8>>>, String> {
    let ledgers = doc
        .get_all(ROOT, "invitation_receipts_v1")
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|(value, object)| match value {
            Value::Object(automerge::ObjType::Map) => Ok(object),
            _ => Err("governed invitation receipt ledger is not a map".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut receipts: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
    for object in ledgers {
        for key in doc.keys(&object) {
            let values = doc
                .get_all(&object, &key)
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|(value, _)| match value {
                    Value::Scalar(value) => match value.as_ref() {
                        ScalarValue::Bytes(bytes) => Ok(bytes.clone()),
                        _ => Err("governed invitation receipt is not bytes".to_string()),
                    },
                    Value::Object(_) => Err("governed invitation receipt is not bytes".to_string()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            receipts.entry(key).or_default().extend(values);
        }
    }
    Ok(receipts)
}

/// Whether every redemption `before` records is still in `after`, byte for
/// byte, under the invitation it redeemed.
///
/// An invitation admits one person: whoever redeems it writes its receipt, and
/// that receipt is the workspace's record from then on. A batch may redeem an
/// invitation the workspace holds no redemption of, and nothing else.
///
/// A ledger this build cannot read is not evidence of either: it keeps nothing.
pub(crate) fn redemptions_kept(before: Option<&AutoCommit>, after: &AutoCommit) -> bool {
    let Some(before) = before else {
        return true;
    };
    let (Ok(before), Ok(after)) = (invitation_receipts(before), invitation_receipts(after)) else {
        return false;
    };
    // An invitation the workspace redeemed keeps exactly its receipts; one it
    // never redeemed may be redeemed now.
    before.iter().all(|(invite, held)| {
        after.get(invite).is_some_and(|written| {
            held.iter().all(|value| written.contains(value))
                && written.iter().all(|value| held.contains(value))
        })
    })
}

/// Every bot of the workspace `doc` governs, by name: each installed bot that
/// was not withdrawn, and each request of a member that no installed bot
/// answers yet. A requested bot the host installed is one bot, marked as
/// requested, so taking the request back is how it is removed. Rows that do
/// not decode are skipped.
#[must_use]
pub fn workspace_bots_of(doc: &AutoCommit) -> Vec<tilo_desktop_model::WorkspaceBot> {
    let requests = crate::state::bot_requests_in(doc);
    let mut bots: Vec<tilo_desktop_model::WorkspaceBot> = governance_of(Some(doc))
        .identities
        .values()
        // ignored-failure: a row that does not decode names no bot, per this reader's contract
        .filter_map(|bytes| crate::state::GovernedIdentity::decode(bytes).ok())
        .filter_map(|identity| {
            let row = identity.delegate?;
            if row.withdrawn_at_ns.is_some() {
                return None;
            }
            let installation = row.installation?;
            let entry = installation
                .source
                .as_ref()
                .map(|source| source.entry.clone());
            let requested = entry.as_ref().is_some_and(|entry| {
                requests
                    .iter()
                    .any(|request| &request.request.entry == entry)
            });
            Some(tilo_desktop_model::WorkspaceBot {
                name: installation.name,
                installation: Some(installation.id.as_str().to_owned()),
                device: Some(hex::encode(identity.device_cert.device_public_key)),
                version: installation.source.map(|source| source.version),
                entry,
                added_by: installation.installed_by,
                requested,
                paused: installation.paused,
            })
        })
        .collect();
    for request in requests {
        if bots
            .iter()
            .any(|bot| bot.entry.as_deref() == Some(request.request.entry.as_str()))
        {
            continue;
        }
        bots.push(tilo_desktop_model::WorkspaceBot {
            name: request.request.entry.clone(),
            installation: None,
            device: None,
            entry: Some(request.request.entry),
            version: Some(request.request.version),
            added_by: request.requested_by,
            requested: true,
            paused: false,
        });
    }
    bots.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.installation.cmp(&b.installation))
    });
    bots
}

/// The workspace's name and authority as `candidate` records them, for the workspace
/// it declares. Rows that do not decode are skipped.
pub fn extract_governed_fields(
    candidate: &AutoCommit,
) -> (Option<String>, tilo_action_policy::WorkspacePolicy) {
    #[cfg(test)]
    POLICY_READS.with(|count| count.set(count.get().saturating_add(1)));
    let text = |key: &str| candidate.str_at(&ROOT, key);
    let workspace = text("room").unwrap_or_default();
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

pub fn candidate(before: Option<&AutoCommit>, changes: &[Vec<u8>]) -> Result<AutoCommit, String> {
    // Each entry is one complete change chunk; their concatenation is ordinary
    // storage, admitted like any other untrusted document mutation.
    let storage = changes.concat();
    let mut candidate = before.cloned().unwrap_or_else(AutoCommit::new);
    candidate.reset_diff_cursor();
    if storage.is_empty() {
        return Ok(candidate);
    }
    // A change under a writer's sequence the document already holds is another
    // branch of that writer, which Automerge refuses as a whole: governance is
    // decided as a whole too, so the batch is refused.
    tilo_automerge_isolate::apply(
        &mut candidate,
        &storage,
        tilo_automerge_isolate::Dependencies::RetainOrphans,
    )
    .map_err(|e| e.to_string())?;
    Ok(candidate)
}
pub fn declared_workspace_mismatches(candidate: &AutoCommit, workspace: uuid::Uuid) -> bool {
    let declared = match candidate.get(ROOT, "room").ok().flatten() {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            // ignored-failure: a workspace id that does not parse declares no other workspace
            ScalarValue::Str(value) => uuid::Uuid::parse_str(value).ok(),
            _ => None,
        },
        _ => None,
    };
    declared.is_some_and(|declared| declared != workspace)
}
/// Whether `after` carries the governance `before` does, compared by value: a
/// peer echoing governance it learned re-states it under its own actor, which
/// is not a change. Durable receipts and import proposals are governed too.
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

/// The workspace's settings as stored: its name and its saved grant sets.
/// Whatever a key holds counts, so a value of another shape than a writer
/// writes is a change too.
fn settings_of(doc: Option<&AutoCommit>) -> Vec<(String, Vec<u8>)> {
    use tilo_desktop_model::projection::{WORKSPACE_NAME_KEY, WORKSPACE_PRESETS_KEY};
    let Some(doc) = doc else {
        return Vec::new();
    };
    let mut settings = Vec::new();
    for key in [WORKSPACE_NAME_KEY, WORKSPACE_PRESETS_KEY] {
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
/// The key holding effect receipts: `{person → {invocation id → JSON receipt}}`.
const EFFECT_RECEIPTS: &str = tilo_desktop_model::projection::WORKSPACE_EFFECT_RECEIPTS_KEY;
/// The key anchoring archived receipts: `{person → {device → root}}`.
const EFFECT_HISTORY: &str = tilo_desktop_model::projection::WORKSPACE_EFFECT_HISTORY_KEY;
/// The key holding write refusals:
/// `{person → {device → {route and author → postcard refusal}}}`.
const WRITE_REFUSALS: &str = tilo_desktop_model::projection::WORKSPACE_WRITE_REFUSALS_KEY;
#[cfg(test)]
thread_local! { pub(crate) static RECEIPT_DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// Decode one stored effect receipt. An unreadable receipt carries no evidence:
/// it is treated as absent everywhere, as it always has been.
fn decode_receipt(bytes: &[u8]) -> Option<serde_json::Value> {
    #[cfg(test)]
    RECEIPT_DECODES.with(|count| count.set(count.get().saturating_add(1)));
    // ignored-failure: an unreadable receipt carries no evidence
    serde_json::from_slice(bytes).ok()
}

/// Every byte value two levels down `key`, `{outer → {inner → bytes}}`,
/// borrowed, with no decoding.
fn visit_nested_bytes<E>(
    doc: &AutoCommit,
    key: &str,
    mut visit: impl FnMut(&str, &str, &[u8]) -> Result<(), E>,
) -> Result<(), E> {
    let Some((Value::Object(_), outer)) = doc.get(ROOT, key).ok().flatten() else {
        return Ok(());
    };
    for first in doc.keys(&outer) {
        let Some((Value::Object(_), inner)) = doc.get(&outer, &first).ok().flatten() else {
            continue;
        };
        for second in doc.keys(&inner) {
            if let Some((Value::Scalar(value), _)) = doc.get(&inner, &second).ok().flatten()
                && let ScalarValue::Bytes(bytes) = value.as_ref()
            {
                visit(&first, &second, bytes)?;
            }
        }
    }
    Ok(())
}

/// Every effect receipt `doc` stores, borrowed, with no decoding: its
/// invocation id and bytes.
fn visit_receipt_bytes<E>(
    doc: &AutoCommit,
    mut visit: impl FnMut(&str, &[u8]) -> Result<(), E>,
) -> Result<(), E> {
    visit_nested_bytes(doc, EFFECT_RECEIPTS, |_, invocation, bytes| {
        visit(invocation, bytes)
    })
}

/// What `doc` holds under `key`, `{outer → {inner → bytes}}`, as stored,
/// with any value of another shape counted too: the slices a member writes.
fn nested_values(doc: Option<&AutoCommit>, key: &str) -> BTreeMap<(String, String), Vec<u8>> {
    let mut values = BTreeMap::new();
    let Some(doc) = doc else {
        return values;
    };
    let Some((top, outer)) = doc.get(ROOT, key).ok().flatten() else {
        return values;
    };
    values.insert((String::new(), String::new()), held_bytes(&top));
    for first in doc.keys(&outer) {
        let Some((value, inner)) = doc.get(&outer, &first).ok().flatten() else {
            continue;
        };
        values.insert((first.clone(), String::new()), held_bytes(&value));
        if matches!(value, Value::Object(_)) {
            for second in doc.keys(&inner) {
                if let Some((value, _)) = doc.get(&inner, &second).ok().flatten() {
                    values.insert((first.clone(), second), held_bytes(&value));
                }
            }
        }
    }
    values
}

/// Whether every effect receipt and history anchor `after` adds or changes
/// against `before` is `person`'s own (none is when no person is known):
/// in their slice, each new receipt
/// naming them or an agent acting for them as its actor, and each anchor
/// in their slice. A receipt is its
/// recorder's: only that member's devices write their slice.
#[must_use]
pub fn effect_slices_kept(
    before: Option<&AutoCommit>,
    after: &AutoCommit,
    person: Option<&[u8; 32]>,
) -> bool {
    let person = person.map(hex::encode).unwrap_or_default();
    let own = |slice: &str| slice.is_empty() || (!person.is_empty() && slice == person);
    let held = nested_values(before, EFFECT_RECEIPTS);
    let receipts = nested_values(Some(after), EFFECT_RECEIPTS);
    for ((slice, invocation), bytes) in &receipts {
        if held.get(&(slice.clone(), invocation.clone())) == Some(bytes) {
            continue;
        }
        if !own(slice) {
            return false;
        }
        if invocation.is_empty() {
            continue;
        }
        let Ok(receipt) =
            serde_json::from_slice::<tilo_desktop_model::WorkspaceEffectReceipt>(bytes)
        else {
            return false;
        };
        if receipt.invocation.0.to_string() != *invocation
            || !(receipt.actor == person || receipt.actor.starts_with(AGENT_ACTOR_PREFIX))
        {
            return false;
        }
    }
    let anchors = nested_values(before, EFFECT_HISTORY);
    let written = nested_values(Some(after), EFFECT_HISTORY);
    anchors
        .keys()
        .chain(written.keys())
        .filter(|key| anchors.get(*key) != written.get(*key))
        .all(|(slice, _)| own(slice))
}

/// Whether every write refusal `after` adds, replaces or clears against
/// `before` is `person`'s own device `device`'s: in that device's slice of the
/// key, and each verdict it writes there signed by it. A refusal is its
/// recorder's verdict; no other device keeps, rewrites or clears it.
#[must_use]
pub fn refusal_slices_kept(
    before: Option<&AutoCommit>,
    after: &AutoCommit,
    person: Option<&[u8; 32]>,
    device: Option<&[u8; 32]>,
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
        if keys.first().map(String::as_str) != Some(WRITE_REFUSALS) {
            // Some other key: other rules decide it. A change to this key's
            // own shape never leaves it, so `nested` false here means it was
            // never this key.
            continue;
        }
        if !nested {
            return false;
        }
        if keys.len() == 1 {
            // The map under the key: every refuser makes it, no one takes it
            // away.
            if !matches!(value, Some(Plain::Object(_))) {
                return false;
            }
            continue;
        }
        let (Some(person), Some(device)) = (person, device) else {
            // A sender the receiver cannot name as a person files no verdict.
            return false;
        };
        if keys.get(1) != Some(&hex::encode(person)) {
            return false;
        }
        if keys.len() >= 3 && keys.get(2) != Some(&hex::encode(device)) {
            return false;
        }
        match keys.len() {
            // The person's own slice, or the device's own within it.
            2 | 3 => {}
            // One verdict, kept or cleared in the device's own slice: a kept
            // one is that device's, over exactly its bytes.
            4 => {
                let Some(Plain::Scalar(value)) = value else {
                    return false;
                };
                let ScalarValue::Bytes(bytes) = value.as_ref() else {
                    return false;
                };
                let Ok(refusal) = postcard::from_bytes::<neighbour_wire::WriteRefusal>(bytes)
                else {
                    return false;
                };
                if refusal.by != *device || !refusal.signed_by(device) {
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
/// refusals ([`refusal_slices_kept`]), its grants, as the shares they give
/// ([`share`], decided as membership), and the membership operations they hold
/// — an operation is its author's, and one who only reads hands on what others
/// signed without admitting it.
const READER_WRITES: [&str; 5] = [
    EFFECT_RECEIPTS,
    EFFECT_HISTORY,
    WRITE_REFUSALS,
    "grants",
    crate::members::MEMBERS_KEY,
];

/// Whether `after` differs from `before` only in what one who only reads
/// the workspace may change in it ([`READER_WRITES`]).
#[must_use]
pub fn only_reader_writes_differ(before: Option<&AutoCommit>, after: &AutoCommit) -> bool {
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
        key.is_some_and(|key| {
            READER_WRITES.contains(&key) || crate::members::op::keyed(key).is_some()
        })
    })
}

/// How an effect receipt names an agent as its actor (`agent:<id>`), as
/// against a person, named by the hex of their key.
pub(crate) const AGENT_ACTOR_PREFIX: &str = "agent:";

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
/// place in `after`; a receipt is decoded only when its bytes changed.
pub fn immutable_receipts(before: Option<&AutoCommit>, after: &AutoCommit) -> bool {
    let Some(before) = before else {
        return true;
    };
    let Some((_, ledger)) = after.get(ROOT, EFFECT_RECEIPTS).ok().flatten() else {
        return true;
    };
    visit_nested_bytes(before, EFFECT_RECEIPTS, |person, invocation, prior| {
        let rewritten = after
            .get(&ledger, person)
            .ok()
            .flatten()
            .and_then(|(_, slice)| after.get(&slice, invocation).ok().flatten())
            .is_some_and(|(value, _)| {
                matches!(value, Value::Scalar(value)
                    if matches!(value.as_ref(), ScalarValue::Bytes(new) if receipt_rewritten(prior, new)))
            });
        if rewritten { Err(()) } else { Ok(()) }
    })
    .is_ok()
}

/// Record every effect receipt `doc` carries in `ledger`, the receipts this
/// peer has admitted for the workspace's lifetime. A receipt the ledger already holds
/// byte-for-byte costs one comparison; a new one is decoded once (an unreadable
/// receipt is not evidence and is not recorded) and stored as received, never
/// re-encoded. Ledger rows written in the older re-encoded form are compared
/// by value and replaced by the received bytes, so they take the fast path next
/// time.
///
/// # Errors
///
/// [`WorkspaceRefusal::ReceiptImmutable`] when `doc` rewrites a receipt the ledger
/// holds; `ledger` may then be partly updated and must be discarded.
pub(crate) fn admit_receipts(
    ledger: &mut BTreeMap<String, Vec<u8>>,
    doc: &AutoCommit,
) -> Result<(), WorkspaceRefusal> {
    visit_receipt_bytes(doc, |key, bytes| {
        let Some(prior) = ledger.get(key) else {
            if decode_receipt(bytes).is_some() {
                ledger.insert(key.to_owned(), bytes.to_vec());
            }
            return Ok(());
        };
        if prior.as_slice() == bytes {
            return Ok(());
        }
        let Some(value) = decode_receipt(bytes) else {
            return Ok(());
        };
        if decode_receipt(prior).is_none_or(|prior| prior != value) {
            return Err(WorkspaceRefusal::ReceiptImmutable);
        }
        ledger.insert(key.to_owned(), bytes.to_vec());
        Ok(())
    })
}

/// Every readable effect receipt in `doc`, decoded.
pub fn receipt_values(doc: &AutoCommit) -> BTreeMap<String, serde_json::Value> {
    let mut values = BTreeMap::new();
    // ignored-failure: the visitor's error type is `Infallible`; an unreadable receipt drops out
    let _ = visit_receipt_bytes(doc, |key, bytes| {
        if let Some(value) = decode_receipt(bytes) {
            values.insert(key.to_owned(), value);
        }
        Ok::<(), std::convert::Infallible>(())
    });
    values
}
