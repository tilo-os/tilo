//! Authenticated applications-only original-source declaration.
//! No Home archive or retained Home access is promised by this type.
use super::*;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OfflineApplicationsPlan {
    schema: u32,
    source: super::super::migration_handoff::Plan,
    export_operation: String,
    pub(super) setup_binding: String,
    pub(super) identity_public_key: [u8; 32],
    media: crate::media_layout::MediaPartitionBinding,
    caller: super::super::home_capture::OriginalCallerFacts,
    sealed_reservation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_proof: Option<super::super::home_capture::SourceProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mounted_proof: Option<super::super::home_capture::MountedProof>,
}
impl OfflineApplicationsPlan {
    pub fn new(
        source: super::super::migration_handoff::Plan,
        setup_binding: String,
        identity_public_key: [u8; 32],
        media: crate::media_layout::MediaPartitionBinding,
        caller: super::super::home_capture::OriginalCallerFacts,
        sealed_reservation: u64,
    ) -> io::Result<Self> {
        let value = Self {
            schema: 1,
            export_operation: source.operation_id.clone(),
            source,
            setup_binding,
            identity_public_key,
            media,
            caller,
            sealed_reservation,
            source_proof: None,
            mounted_proof: None,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn with_source_proof(
        mut self,
        proof: super::super::home_capture::SourceProof,
    ) -> io::Result<Self> {
        proof.validate_plan(&self.source)?;
        self.source_proof = Some(proof);
        self.validate()?;
        Ok(self)
    }
    pub fn source_proof(&self) -> Option<&super::super::home_capture::SourceProof> {
        self.source_proof.as_ref()
    }
    pub fn with_mounted_proof(
        mut self,
        proof: super::super::home_capture::MountedProof,
    ) -> io::Result<Self> {
        proof.matches_source(self.source_proof.as_ref().ok_or_else(invalid)?)?;
        self.mounted_proof = Some(proof);
        self.validate()?;
        Ok(self)
    }
    pub fn mounted_proof(&self) -> Option<&super::super::home_capture::MountedProof> {
        self.mounted_proof.as_ref()
    }
    pub(super) fn validate(&self) -> io::Result<()> {
        self.source.validate()?;
        let inventory = self.source.flatpak_inventory.as_ref().ok_or_else(invalid)?;
        if super::super::migration_handoff::OriginalInventoryBinding::parse(&self.source)?.is_none()
        {
            let inventory = super::super::flatpak_transfer::HandoffInventory::parse(
                &serde_json::to_vec(inventory)?,
            )
            .map_err(io::Error::other)?;
            inventory
                .validate_operation(&self.source.operation_id)
                .map_err(io::Error::other)?;
            if !inventory
                .selected()
                .iter()
                .any(|item| item.app_id().is_some())
            {
                return Err(invalid());
            }
        }
        self.caller.validate(self.source.home.original_uid())?;
        if let Some(proof) = &self.source_proof {
            proof.validate_plan(&self.source)?;
        }
        if let Some(proof) = &self.mounted_proof {
            proof.matches_source(self.source_proof.as_ref().ok_or_else(invalid)?)?;
        }
        let uuid = |value: &str| {
            uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
        };
        if self.schema != 1
            || self.caller.uid() != self.source.home.original_uid()
            || self.source.home_policy != super::super::migration_handoff::HomePolicy::Copy
            || self.source.categories.home
            || !self.source.categories.flatpak
            || (self.source.categories.flatpak && self.source.flatpak_inventory.is_none())
            || self.source.operation_id != self.export_operation
            || !crate::is_operation_id(&self.export_operation)
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.setup_binding.chars().any(char::is_control)
            || self.media.schema != 1
            || self.media.target_stable_id.is_empty()
            || self.media.target_stable_id.len() > 256
            || self.media.target_stable_id.chars().any(char::is_control)
            || !uuid(&self.media.disk_guid)
            || !uuid(&self.media.partition_guid)
            || self.media.number == 0
            || self.sealed_reservation == 0
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn applications_operation(&self) -> io::Result<String> {
        self.validate()?;
        Ok(
            super::super::home_capture::OriginalReadClaim::derive_applications_operation(
                &self.source.operation_id,
                &self.setup_binding,
                &self.identity_public_key,
            ),
        )
    }
    pub fn setup_binding(&self) -> &str {
        &self.setup_binding
    }
    pub fn identity_public_key(&self) -> [u8; 32] {
        self.identity_public_key
    }
    pub fn canonical_digest(&self) -> io::Result<[u8; 32]> {
        self.validate()?;
        Ok(Sha256::digest(serde_json::to_vec(self)?).into())
    }
    pub fn requested_caller(&self) -> &super::super::home_capture::OriginalCallerFacts {
        &self.caller
    }
    pub fn source(&self) -> &super::super::migration_handoff::Plan {
        &self.source
    }
    pub fn media(&self) -> &crate::media_layout::MediaPartitionBinding {
        &self.media
    }
    /// Reconstruct the producer's complete geometry from the authenticated
    /// declaration and, for selected installations, its genuine source ownership.
    /// Protected marker JSON alone never supplies an installation inventory.
    #[cfg(target_os = "linux")]
    pub fn authenticated_source_plans(&self) -> io::Result<Vec<Vec<u8>>> {
        let geometry = crate::setup::source_geometry::original_source_plans;
        if !self.source.categories.flatpak && self.mounted_proof.is_none() {
            return geometry(&self.source);
        }
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Original installation geometry requires root ownership",
            ));
        }
        let proof = self.source_proof().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Original installation geometry lacks source proof",
            )
        })?;
        let source = super::super::migration_handoff::mount_read_only(&self.source)?;
        // Retain the actual owned resolver while record authentication runs:
        // a construction error must not fall through an unobserved mount Drop.
        let original = match super::super::home_capture::SourceReadContext::review_proof(
            source.clone(),
            proof,
        ) {
            Ok(original) => {
                drop(source);
                original
            }
            Err(error) => {
                source.retire()?;
                return Err(error);
            }
        };
        let result = (|| {
            original.verify_requested(&self.source, proof, &self.caller)?;
            let mut plans = if self.source.categories.flatpak {
                let inventory =
                    super::super::flatpak_transfer::offline_original::OriginalInventoryGrant::review(
                        &original,
                    )?
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "Selected original installation geometry lacks protected inventory",
                        )
                    })?;
                inventory.matches_applications_plan(&self.source)?;
                let mut expanded = self.source.clone();
                expanded.flatpak_inventory = Some(serde_json::to_value(inventory.inventory())?);
                let plans = geometry(&expanded)?;
                inventory.matches_applications_plan(&self.source)?;
                plans
            } else {
                geometry(&self.source)?
            };
            if let Some(mounted) = self.mounted_proof() {
                // This API is a bounded metadata-only review with no job token.
                // The export path uses its live token and retained epoch instead.
                let cancel = std::sync::atomic::AtomicBool::new(false);
                original.append_mounted_source_geometry(mounted, &mut plans, &cancel)?;
            }
            original.verify_requested(&self.source, proof, &self.caller)?;
            Ok(plans)
        })();
        // All borrowed inventory descriptors are gone before mount retirement.
        // Both successful and failed geometry reviews confirm owned cleanup.
        original.retire()?;
        result
    }
    pub fn sealed_reservation(&self) -> u64 {
        self.sealed_reservation
    }
}
/// Internal authority selector; canonical bytes are always the inner declaration,
/// preserving the exact existing Home wire and its authenticated digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginalSelectionPlan {
    Home(OfflineHomePlan),
    Applications(OfflineApplicationsPlan),
}
impl OriginalSelectionPlan {
    pub fn bytes(&self) -> io::Result<Vec<u8>> {
        match self {
            Self::Home(plan) => {
                plan.validate()?;
                Ok(serde_json::to_vec(plan)?)
            }
            Self::Applications(plan) => {
                plan.validate()?;
                Ok(serde_json::to_vec(plan)?)
            }
        }
    }
    pub fn canonical_digest(&self) -> io::Result<[u8; 32]> {
        Ok(Sha256::digest(self.bytes()?).into())
    }
    pub fn as_home(&self) -> io::Result<&OfflineHomePlan> {
        match self {
            Self::Home(plan) => Ok(plan),
            _ => Err(invalid()),
        }
    }
    pub fn as_applications(&self) -> io::Result<&OfflineApplicationsPlan> {
        match self {
            Self::Applications(plan) => Ok(plan),
            _ => Err(invalid()),
        }
    }
    pub fn is_applications_only(&self) -> bool {
        matches!(self, Self::Applications(_))
    }
    pub fn setup_binding(&self) -> &str {
        match self {
            Self::Home(plan) => &plan.setup_binding,
            Self::Applications(plan) => plan.setup_binding(),
        }
    }
    pub fn identity_public_key(&self) -> [u8; 32] {
        match self {
            Self::Home(plan) => plan.identity_public_key,
            Self::Applications(plan) => plan.identity_public_key(),
        }
    }
    pub fn source(&self) -> &super::super::migration_handoff::Plan {
        match self {
            Self::Home(plan) => plan.source(),
            Self::Applications(plan) => plan.source(),
        }
    }
    pub fn media(&self) -> &crate::media_layout::MediaPartitionBinding {
        match self {
            Self::Home(plan) => plan.media(),
            Self::Applications(plan) => plan.media(),
        }
    }
    pub fn requested_caller(&self) -> &super::super::home_capture::OriginalCallerFacts {
        match self {
            Self::Home(plan) => plan.requested_caller(),
            Self::Applications(plan) => plan.requested_caller(),
        }
    }
    pub fn source_proof(&self) -> Option<&super::super::home_capture::SourceProof> {
        match self {
            Self::Home(plan) => plan.source_proof(),
            Self::Applications(plan) => plan.source_proof(),
        }
    }
    pub fn mounted_proof(&self) -> Option<&super::super::home_capture::MountedProof> {
        match self {
            Self::Home(plan) => plan.mounted_proof(),
            Self::Applications(plan) => plan.mounted_proof(),
        }
    }
    pub fn sealed_reservation(&self) -> u64 {
        match self {
            Self::Home(plan) => plan.sealed_reservation(),
            Self::Applications(plan) => plan.sealed_reservation(),
        }
    }
    pub fn authenticated_source_plans(&self) -> io::Result<Vec<Vec<u8>>> {
        match self {
            Self::Home(plan) => plan.authenticated_source_plans(),
            Self::Applications(plan) => plan.authenticated_source_plans(),
        }
    }
}
