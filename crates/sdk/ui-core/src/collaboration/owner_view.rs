//! Session-local native owner-view control. Documents retain their normal CRDT
//! routes; these messages carry presentation only and grant no focus or input.
use super::ContinuationMetadata;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerView {
    pub room: String,
    pub app: String,
    pub instance: u64,
    pub generation: u64,
    pub counter: u64,
    pub source: String,
    pub projection: Option<OwnerProjection>,
    /// A local takeover is acknowledged only after the host records its
    /// newer Lamport revision. Older queued remote views then remain stale.
    pub local_claim: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerProjection {
    pub schema_version: u16,
    pub bytes: Vec<u8>,
}

impl OwnerView {
    pub fn valid(&self) -> bool {
        self.generation > 0
            && self
                .room
                .parse::<uuid::Uuid>()
                .is_ok_and(|room| !room.is_nil())
            && !self.app.is_empty()
            && self.app.len() <= 256
            && self.source.len() <= 256
            && !self.source.is_empty()
            && self
                .source
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            && self.projection.as_ref().is_none_or(|metadata| {
                metadata.schema_version > 0
                    && !metadata.bytes.is_empty()
                    && metadata.bytes.len() <= 32 * 1024
            })
    }
}

/// Device-local lifetime; never persisted as document state.
#[derive(Default)]
pub(crate) struct OwnerViewRuntime {
    pub(crate) last_generation: u64,
    pub(crate) last_claim_at: Option<crate::time::Instant>,
    pub(crate) last_stamp: Option<(u64, String)>,
    pub(crate) pending: Option<ContinuationMetadata>,
    pub(crate) following: bool,
    pub(crate) awaiting_claim: bool,
}
