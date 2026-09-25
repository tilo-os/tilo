//! Actual core metadata adapter to the one strict private policy engine.
use super::*;
use crate::state::bounded::{BoundedPeerError, BoundedPeerInvalid, PeerLimits};
use automerge::resource_limits::NativeWorkTurn;
pub(in crate::state) fn bounded(
    core: &PeerCore<BoundedNative>,
    turn: &NativeWorkTurn,
    limits: PeerLimits,
) -> Result<crate::room::private_policy::NativeBasis, BoundedPeerError> {
    use crate::room::private_policy::{Error, Limits, Metadata};
    turn.reserve(0).map_err(BoundedPeerError::Resource)?;
    let room = core
        .room_doc
        .as_ref()
        .ok_or(BoundedPeerError::Invalid(BoundedPeerInvalid::Governance))?;
    let read = room.read(turn).map_err(BoundedPeerError::Resource)?;
    crate::room::private_policy::native(
        &read,
        Metadata {
            room: &core.room,
            authority: &core.authority,
            certificates: core
                .certificates
                .map()
                .values()
                .map(|certificate| certificate.as_ref()),
            revocations: core.revocations.iter(),
        },
        turn,
        Limits {
            collections: limits.governance_collection_bytes,
            json: crate::resources::json::JsonLimits {
                allocation_bytes: limits.governance_json.allocation_bytes,
                collection_entries: limits.governance_json.collection_entries,
            },
        },
    )
    .map_err(|error| match error {
        Error::Resource(error) => BoundedPeerError::Resource(error),
        Error::Decode(error) => BoundedPeerError::Decode(error),
        _ => BoundedPeerError::Invalid(BoundedPeerInvalid::Governance),
    })
}

// These existing StatePeer surfaces remain explicitly allocating and unqualified.
// They supply metadata from their own core, never caller-selected policy rows.
impl StatePeer {
    pub(crate) fn legacy_private_policy_basis(
        &self,
    ) -> Result<crate::room::private_policy::LegacyBasis, String> {
        use crate::room::{
            private_policy::{self, Metadata},
            read::LegacyRead,
        };
        let doc = self
            .core
            .room_doc
            .as_deref()
            .ok_or("private policy room unavailable")?;
        private_policy::legacy(
            &LegacyRead(doc),
            Metadata {
                room: &self.core.room,
                authority: &self.core.authority,
                certificates: self
                    .core
                    .certificates
                    .values()
                    .map(|certificate| certificate.as_ref()),
                revocations: self.core.revocations.iter(),
            },
        )
        .map_err(private_policy::Error::legacy)
    }
    pub(crate) fn legacy_canonical_owner(&self) -> Result<String, String> {
        use crate::room::{private_policy, read::LegacyRead};
        let doc = self
            .core
            .room_doc
            .as_deref()
            .ok_or("admitted room missing")?;
        private_policy::legacy_owner(&LegacyRead(doc), &self.core.room, &self.core.authority)
            .map_err(private_policy::Error::legacy)
    }
    pub(crate) fn legacy_governance_revision(&self) -> Result<String, String> {
        use crate::room::{
            private_policy::{self, Metadata},
            read::LegacyRead,
        };
        let doc = self
            .core
            .room_doc
            .as_deref()
            .ok_or("admitted room missing")?;
        private_policy::legacy_revision(
            &LegacyRead(doc),
            Metadata {
                room: &self.core.room,
                authority: &self.core.authority,
                certificates: self
                    .core
                    .certificates
                    .values()
                    .map(|certificate| certificate.as_ref()),
                revocations: self.core.revocations.iter(),
            },
        )
        .map_err(private_policy::Error::legacy)
    }
}
