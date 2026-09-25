//! Minimal, canonical typed owner receipt stored in the existing consumed map.
//!
//! One receipt per owner device (`owner-membership-v2:<device hex>`): it binds
//! the last completed request ID and the exact proof digest to the Store
//! generation that completed it. It deliberately never retains the reply: a
//! lost-reply retry is answered from current state (see `owner_bootstrap.rs`).
//! Legacy `owner-membership-v1:*` receipts (the whole retained bootstrap) are
//! pruned on the next Store write.
use super::*;
use serde_json::Value;

pub(super) const PREFIX: &str = "owner-membership-v2:";
pub(super) const LEGACY_PREFIX: &str = "owner-membership-v1:";
/// A lost-reply retry of the same request is answered only this long after
/// its completion; a later retry is refused (the browser starts a fresh
/// request ID for every refresh anyway).
pub(super) const REPLAY_WINDOW_NS: u64 = 600_000_000_000;

pub(super) fn key(device: [u8; 32]) -> String {
    format!("{PREFIX}{}", hex::encode(device))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReceiptPurpose {
    OwnerMembership,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnerReceipt {
    pub version: u32,
    pub purpose: ReceiptPurpose,
    pub device: [u8; 32],
    pub request_id: [u8; 32],
    pub proof_digest: [u8; 32],
    pub generation: u64,
    pub completed_at_ns: u64,
}

impl OwnerReceipt {
    fn validate(&self) -> Result<()> {
        if self.version != 2 {
            bail!("owner receipt version is not supported")
        }
        if self.request_id == [0; 32] || self.generation == 0 {
            bail!("owner receipt binding is absent")
        }
        Ok(())
    }
    pub(super) fn encode(&self) -> Result<Value> {
        self.validate()?;
        Ok(serde_json::to_value(self)?)
    }
    /// Exact canonical decode: the value is small and fixed-shape, so any
    /// unknown field, alternative representation or size outlier is refused.
    pub(super) fn decode(value: &Value) -> Result<Self> {
        if serde_json::to_vec(value)?.len() > 1024 {
            bail!("owner receipt size limit")
        }
        let receipt = Self::deserialize(value)?;
        receipt.validate()?;
        if receipt.encode()? != *value {
            bail!("owner receipt is not canonical")
        }
        Ok(receipt)
    }
}
