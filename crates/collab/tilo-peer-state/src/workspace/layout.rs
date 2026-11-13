//! The one step that brings a workspace's governance written in the layout
//! before this one up to it.
//!
//! The earlier layout kept each device row as JSON, every key and signature
//! a list of decimal numbers, and each invitation receipt with its whole
//! signed invitation as base64 text: about four kilobytes a member, which
//! every newcomer to a workspace of hundreds downloaded. This layout keeps a
//! row as postcard ([`crate::state::GovernedIdentity::encode`]) and a
//! receipt with its invitation's shared runs named once
//! ([`crate::state::invitation_ledger`]).
//!
//! A document is brought up once, as its durable state is read
//! ([`crate::StatePeer::restore`]), into a base of the next generation: what
//! the earlier layout wrote is in no history the document keeps from then
//! on, and nothing reads it again. Every value the step rewrites reads back
//! as what it held, so the checks a device makes on the governance, its
//! certificates, signatures and receipts, decide as they did.
use automerge::hydrate::{Map, Value};
use automerge::{ActorId, AutoCommit, ScalarValue};
use serde::Deserialize;
use std::collections::HashMap;
use tilo_user_identity::DeviceCert;

use crate::state::invitation_ledger::{LedgerReceipt, PARTS_KEY, RECEIPTS_KEY, part_key};
use crate::state::{GovernedIdentity, RedemptionReceipt};

/// The document key holding the device rows.
const IDENTITIES_KEY: &str = "peer_identities_v1";

/// A device row as the earlier layout wrote it.
#[derive(Deserialize)]
struct RowBefore {
    device_cert: DeviceCert,
    #[serde(default)]
    revocations: Vec<tilo_user_identity::RevocationEntry>,
    #[serde(default)]
    admission: Option<ReceiptBefore>,
    #[serde(default)]
    admissions: Vec<ReceiptBefore>,
    #[serde(default)]
    delegate: Option<crate::state::GovernedDelegateRow>,
    #[serde(default)]
    workspace_delegations: Vec<crate::state::WorkspaceDelegation>,
}

/// A redemption receipt as the earlier layout wrote it.
#[derive(Deserialize)]
struct ReceiptBefore {
    invite_id: [u8; 32],
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    request_id: [u8; 32],
    role: crate::join::MemberRole,
    /// The signed invitation as base64 text, in the ledger's copy.
    #[serde(default)]
    invitation: Option<String>,
}

impl ReceiptBefore {
    fn receipt(&self) -> RedemptionReceipt {
        RedemptionReceipt {
            invite_id: self.invite_id,
            user_public_key: self.user_public_key,
            device_public_key: self.device_public_key,
            request_id: self.request_id,
            role: self.role,
        }
    }
}

/// `doc`, a governance document of the earlier layout, as a base of the next
/// generation by `actor` in this one. A row or receipt that does not read
/// as the earlier layout is kept as it was: it read as nothing before, and
/// reads as nothing now.
pub(crate) fn bring_up(doc: &AutoCommit, actor: ActorId) -> Result<AutoCommit, String> {
    super::base::rebase_with(doc, actor, |state| {
        if let Some(Value::Map(rows)) = state.get_mut(IDENTITIES_KEY) {
            for entry in rows.values_mut() {
                if let Value::Scalar(ScalarValue::Bytes(bytes)) = &mut entry.value
                    && let Some(row) = row(bytes)
                {
                    *bytes = row;
                }
            }
        }
        let mut parts = HashMap::new();
        if let Some(Value::Map(receipts)) = state.get_mut(RECEIPTS_KEY) {
            for entry in receipts.values_mut() {
                if let Value::Scalar(ScalarValue::Bytes(bytes)) = &mut entry.value
                    && let Some((receipt, shared)) = receipt(bytes)
                {
                    *bytes = receipt;
                    for part in shared {
                        parts.insert(
                            part_key(blake3::hash(&part).as_bytes()),
                            Value::Scalar(ScalarValue::Bytes(part)),
                        );
                    }
                }
            }
        }
        if !parts.is_empty() {
            state.insert(
                PARTS_KEY.to_owned(),
                automerge::hydrate::MapValue {
                    value: Value::Map(Map::from(parts)),
                    conflict: false,
                },
            );
        }
    })
    .map_err(|error| error.to_string())
}

/// The row `bytes` held, in this layout.
fn row(bytes: &[u8]) -> Option<Vec<u8>> {
    let before: RowBefore = serde_json::from_slice(bytes).ok()?;
    GovernedIdentity {
        device_cert: before.device_cert,
        revocations: before.revocations,
        admission: before.admission.as_ref().map(ReceiptBefore::receipt),
        admissions: before
            .admissions
            .iter()
            .map(ReceiptBefore::receipt)
            .collect(),
        delegate: before.delegate,
        workspace_delegations: before.workspace_delegations,
    }
    .encode()
    .ok()
}

/// The ledger receipt `bytes` held, in this layout, and the parts it names.
fn receipt(bytes: &[u8]) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
    use base64::Engine as _;
    let before: ReceiptBefore = serde_json::from_slice(bytes).ok()?;
    let text = before.invitation.as_deref().unwrap_or_default();
    let signed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .ok()?;
    let shared = crate::join::invitation_shared_runs(&signed);
    let (ledgered, parts) = LedgerReceipt::new(
        before.receipt(),
        &signed,
        &shared.iter().map(Vec::as_slice).collect::<Vec<_>>(),
    );
    Some((ledgered.encode().ok()?, parts))
}

#[cfg(test)]
#[path = "layout_tests.rs"]
pub(crate) mod tests;
