//! Actual core metadata adapter to the one strict private policy engine.
use super::{BoundedNative, PeerCore, StatePeer};
use crate::state::bounded::{BoundedPeerError, BoundedPeerInvalid, PeerLimits};
pub(in crate::state) fn bounded(
    core: &PeerCore<BoundedNative>,
    limits: PeerLimits,
) -> Result<crate::room::private_policy::NativeBasis, BoundedPeerError> {
    use crate::room::private_policy::{Error, Limits, Metadata};
    let room = core
        .room_doc
        .as_ref()
        .ok_or(BoundedPeerError::Invalid(BoundedPeerInvalid::Governance))?;
    let read = room.read();
    crate::room::private_policy::native(
        &read,
        Metadata {
            room: &core.room,
            authority: &core.authority,
            certificates: core
                .certificates
                .map()
                .values()
                .map(std::convert::AsRef::as_ref),
            revocations: core.revocations.iter(),
        },
        Limits {
            collections: limits.governance_collection_bytes,
            json: crate::resources::json::JsonLimits {
                allocation_bytes: limits.governance_json.allocation_bytes,
                collection_entries: limits.governance_json.collection_entries,
            },
        },
    )
    .map_err(|error| match error {
        Error::Capacity => BoundedPeerError::Capacity,
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
        use crate::room::{private_policy, read::LegacyRead};
        let doc = self
            .core
            .room_doc
            .as_deref()
            .ok_or("private policy room unavailable")?;
        private_policy::legacy(&LegacyRead(doc), self.legacy_metadata())
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
        use crate::room::{private_policy, read::LegacyRead};
        let doc = self
            .core
            .room_doc
            .as_deref()
            .ok_or("admitted room missing")?;
        private_policy::legacy_revision(&LegacyRead(doc), self.legacy_metadata())
            .map_err(private_policy::Error::legacy)
    }
    fn legacy_metadata(
        &self,
    ) -> crate::room::private_policy::Metadata<
        '_,
        impl ExactSizeIterator<Item = &tilo_user_identity::DeviceCert> + Clone,
        impl Iterator<Item = &tilo_user_identity::RevocationEntry>,
    > {
        crate::room::private_policy::Metadata {
            room: &self.core.room,
            authority: &self.core.authority,
            certificates: self
                .core
                .certificates
                .values()
                .map(std::convert::AsRef::as_ref),
            revocations: self.core.revocations.iter(),
        }
    }
}
