//! Actual core metadata adapter to the one strict private policy engine.
use super::StatePeer;

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
        private_policy::legacy_owner(&LegacyRead(doc), &self.core.room, &self.core.owner)
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
            owner: &self.core.owner,
            certificates: self
                .core
                .certificates
                .values()
                .map(std::convert::AsRef::as_ref),
            revocations: self.core.revocations.iter(),
        }
    }
}
