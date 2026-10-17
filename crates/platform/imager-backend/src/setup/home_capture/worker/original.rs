//! Original-source read claim; only the root-admitted launcher grants access.
//! No snapshot identity, output capability, password or completed receipt exists here.
use super::*;
use crate::setup::home_capture::OriginalCallerFacts;
use serde::{Deserialize, Serialize};
#[cfg(feature = "capture-worker")]
use std::sync::Arc;
use std::{fs::File, os::unix::fs::MetadataExt};

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalReadClaim {
    schema: u32,
    #[serde(default, skip_serializing_if = "OriginalMaterial::is_home")]
    material: OriginalMaterial,
    operation: String,
    setup_binding: String,
    identity_public_key: [u8; 32],
    source_plan_sha256: [u8; 32],
    declaration_sha256: [u8; 32],
    source: (u64, u64, u32),
    caller: OriginalCallerFacts,
    export_operation: String,
    sealed_reservation: u64,
    omissions: Vec<crate::setup::migration_review::OriginalOmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    flatpak: Option<crate::setup::flatpak_transfer::offline_original::OriginalFlatpakClaim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    applications_operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mounted: Option<OriginalMountedClaim>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalMountedClaim {
    witness: super::super::MountedWitness,
    table_sha256: [u8; 32],
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginalReadClaimWire {
    schema: u32,
    #[serde(default)]
    material: OriginalMaterial,
    operation: String,
    setup_binding: String,
    identity_public_key: [u8; 32],
    source_plan_sha256: [u8; 32],
    declaration_sha256: [u8; 32],
    source: (u64, u64, u32),
    caller: OriginalCallerFacts,
    export_operation: String,
    sealed_reservation: u64,
    omissions: Vec<crate::setup::migration_review::OriginalOmission>,
    #[serde(default)]
    flatpak: Option<crate::setup::flatpak_transfer::offline_original::OriginalFlatpakClaim>,
    #[serde(default)]
    applications_operation: Option<String>,
    #[serde(default)]
    mounted: Option<OriginalMountedClaim>,
}
impl TryFrom<OriginalReadClaimWire> for OriginalReadClaim {
    type Error = io::Error;

    fn try_from(wire: OriginalReadClaimWire) -> io::Result<Self> {
        let value = Self {
            schema: wire.schema,
            material: wire.material,
            operation: wire.operation,
            setup_binding: wire.setup_binding,
            identity_public_key: wire.identity_public_key,
            source_plan_sha256: wire.source_plan_sha256,
            declaration_sha256: wire.declaration_sha256,
            source: wire.source,
            caller: wire.caller,
            export_operation: wire.export_operation,
            sealed_reservation: wire.sealed_reservation,
            omissions: wire.omissions,
            flatpak: wire.flatpak,
            applications_operation: wire.applications_operation,
            mounted: wire.mounted,
        };
        // Wire facts validate shape only; source admission still requires the
        // launcher's authenticated proof and the held readonly source.
        value.validate_shape(&value.operation)?;
        Ok(value)
    }
}
impl<'de> Deserialize<'de> for OriginalReadClaim {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(OriginalReadClaimWire::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginalMountedClaimWire {
    witness: super::super::MountedWitness,
    table_sha256: [u8; 32],
}
impl TryFrom<OriginalMountedClaimWire> for OriginalMountedClaim {
    type Error = io::Error;

    fn try_from(wire: OriginalMountedClaimWire) -> io::Result<Self> {
        wire.witness.matches_source(wire.witness.primary())?;
        super::super::operation(wire.witness.operation())?;
        if wire.table_sha256 == [0; 32] {
            return Err(failure("Original mounted table digest missing"));
        }
        Ok(Self {
            witness: wire.witness,
            table_sha256: wire.table_sha256,
        })
    }
}
impl<'de> Deserialize<'de> for OriginalMountedClaim {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(OriginalMountedClaimWire::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

impl OriginalMountedClaim {
    pub(in crate::setup::home_capture) fn admitted(
        witness: &super::super::MountedWitness,
        table_sha256: [u8; 32],
    ) -> io::Result<Self> {
        if rustix::process::geteuid().as_raw() != 0 || table_sha256 == [0; 32] {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(Self {
            witness: witness.clone(),
            table_sha256,
        })
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum OriginalMaterial {
    #[default]
    Home,
    SelectedApplications,
}
impl OriginalMaterial {
    fn is_home(&self) -> bool {
        matches!(self, Self::Home)
    }
}
impl OriginalReadClaim {
    /// A root-only original read admission calls this while its setup, medium,
    /// protected source record and readonly mount are held and reattested.
    pub(in crate::setup::home_capture) fn admitted(
        plan: &crate::setup::prepared_setup::OriginalSelectionPlan,
        source: &File,
        setup_binding: &str,
        identity_public_key: [u8; 32],
        omissions: &[crate::setup::migration_review::OriginalOmission],
        flatpak: Option<&crate::setup::flatpak_transfer::offline_original::OriginalFlatpakClaim>,
        mounted: Option<&OriginalMountedClaim>,
    ) -> io::Result<Self> {
        use sha2::{Digest, Sha256};
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if plan.source_witness().is_none() {
            return Err(failure("Original source witness missing"));
        }
        match (plan.mounted_witness(), mounted) {
            (Some(expected), Some(actual)) if expected == &actual.witness => {
                actual.witness.matches_source(
                    plan.source_witness()
                        .ok_or_else(|| failure("Mounted original source missing"))?,
                )?;
            }
            (None, None) => {}
            _ => {
                return Err(failure(
                    "Original mounted claim differs from authenticated declaration",
                ));
            }
        }
        let declaration = plan.bytes()?;
        if declaration.len() > 64 * 1024 {
            return Err(failure("Original declaration exceeds bound"));
        }
        let metadata = source.metadata()?;
        let value = Self {
            schema: if plan.is_applications_only() { 3 } else { 2 },
            material: if plan.is_applications_only() {
                OriginalMaterial::SelectedApplications
            } else {
                OriginalMaterial::Home
            },
            mounted: mounted.cloned(),
            operation: plan.source().operation_id.clone(),
            setup_binding: setup_binding.into(),
            identity_public_key,
            source_plan_sha256: Sha256::digest(plan.source().bytes()?).into(),
            declaration_sha256: Sha256::digest(declaration).into(),
            source: (metadata.dev(), metadata.ino(), metadata.uid()),
            caller: plan.requested_caller().clone(),
            export_operation: plan.source().operation_id.clone(),
            sealed_reservation: plan.sealed_reservation(),
            omissions: omissions.to_vec(),
            flatpak: flatpak.cloned(),
            applications_operation: flatpak.map(|_| {
                Self::derive_applications_operation(
                    &plan.source().operation_id,
                    setup_binding,
                    &identity_public_key,
                )
            }),
        };
        value.validate_shape(&value.operation)?;
        value.validate_source(source)?;
        Ok(value)
    }
}
impl OriginalReadClaim {
    /// A root-only original read admission calls this while its setup, medium,
    /// protected source record and readonly mount are held and reattested.
    pub(in crate::setup::home_capture) fn admitted_copy(
        plan: &crate::setup::prepared_setup::OriginalCopyDeclaration,
        source: &File,
        setup_binding: &str,
        identity_public_key: [u8; 32],
        omissions: &[crate::setup::migration_review::OriginalOmission],
        flatpak: Option<&crate::setup::flatpak_transfer::offline_original::OriginalFlatpakClaim>,
        mounted: Option<&OriginalMountedClaim>,
    ) -> io::Result<Self> {
        use sha2::{Digest, Sha256};
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        plan.validate()?;
        if setup_binding != plan.setup_binding()
            || identity_public_key != plan.identity_public_key()
        {
            return Err(failure("Installed copy setup binding differs"));
        }
        match (plan.mounted_witness(), mounted) {
            (Some(expected), Some(actual)) if expected == &actual.witness => {
                actual.witness.matches_source(plan.source_witness())?;
            }
            (None, None) => {}
            _ => {
                return Err(failure(
                    "Original mounted claim differs from authenticated declaration",
                ));
            }
        }
        let declaration = plan.bytes()?;
        if declaration.len() > 64 * 1024 {
            return Err(failure("Original declaration exceeds bound"));
        }
        let metadata = source.metadata()?;
        let value = Self {
            schema: if plan.source().categories.home { 2 } else { 3 },
            material: if plan.source().categories.home {
                OriginalMaterial::Home
            } else {
                OriginalMaterial::SelectedApplications
            },
            mounted: mounted.cloned(),
            operation: plan.source().operation_id.clone(),
            setup_binding: setup_binding.into(),
            identity_public_key,
            source_plan_sha256: Sha256::digest(plan.source().bytes()?).into(),
            declaration_sha256: Sha256::digest(declaration).into(),
            source: (metadata.dev(), metadata.ino(), metadata.uid()),
            caller: plan.requested_caller().clone(),
            export_operation: plan.source().operation_id.clone(),
            sealed_reservation: plan.reservation(),
            omissions: omissions.to_vec(),
            flatpak: flatpak.cloned(),
            applications_operation: flatpak.map(|_| {
                Self::derive_applications_operation(
                    &plan.source().operation_id,
                    setup_binding,
                    &identity_public_key,
                )
            }),
        };
        value.validate_shape(&value.operation)?;
        value.validate_source(source)?;
        Ok(value)
    }
    /// Exact read-request binding for the root bootstrap and a future distinct
    /// output transaction. This is a digest of public authority facts, not Home.
    pub fn binding_sha256(&self) -> io::Result<[u8; 32]> {
        use sha2::{Digest, Sha256};
        self.validate_shape(&self.operation)?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > 64 * 1024 {
            return Err(failure("Original claim exceeds bound"));
        }
        Ok(Sha256::digest(bytes).into())
    }
    pub(in crate::setup::home_capture) fn mounted_binding(
        &self,
    ) -> io::Result<Option<launcher::owned_service::OriginalMountedBinding>> {
        self.mounted
            .as_ref()
            .map(|mounted| {
                Ok(launcher::owned_service::OriginalMountedBinding {
                    read_claim_digest: self.binding_sha256()?,
                    table_sha256: mounted.table_sha256,
                })
            })
            .transpose()
    }
    pub(super) fn validate_shape(&self, operation: &str) -> io::Result<()> {
        super::super::operation(&self.operation)?;
        self.caller.validate(self.source.2)?;
        crate::setup::migration_review::OriginalOmission::validate_shapes(&self.omissions)?;
        if let Some(mounted) = &self.mounted {
            if mounted.table_sha256 == [0; 32] || mounted.witness.operation() != operation {
                return Err(failure("Original mounted claim shape differs"));
            }
        }
        if self.flatpak.is_some() != self.applications_operation.is_some() {
            return Err(failure("Original application operation admission missing"));
        }
        if let Some(applications_operation) = &self.applications_operation {
            if applications_operation
                != &Self::derive_applications_operation(
                    &self.operation,
                    &self.setup_binding,
                    &self.identity_public_key,
                )
            {
                return Err(failure("Original application operation differs"));
            }
        }
        if let Some(flatpak) = &self.flatpak {
            flatpak.validate_shape(operation)?;
            if flatpak.original_uid() != self.caller.uid() {
                return Err(failure("Original application source owner differs"));
            }
        }
        if self.schema != if self.is_applications_only() { 3 } else { 2 }
            || (self.is_applications_only() && self.flatpak.is_none())
            || self.flatpak.as_ref().is_some_and(|claim| {
                claim.selected_data_ids().is_some() != self.is_applications_only()
            })
            || self.export_operation != self.operation
            || self.sealed_reservation == 0
            || self.operation != operation
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.setup_binding.chars().any(char::is_control)
            || self.identity_public_key == [0; 32]
            || self.source_plan_sha256 == [0; 32]
            || self.declaration_sha256 == [0; 32]
        {
            return Err(failure("Original read command binding differs"));
        }
        Ok(())
    }
    fn validate_source(&self, source: &File) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let facts = source.metadata()?;
        if !facts.is_dir() || (facts.dev(), facts.ino(), facts.uid()) != self.source {
            return Err(failure("Original source projection differs"));
        }
        let mut flags: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(source.as_raw_fd(), &raw mut flags) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags.f_flag & libc::ST_RDONLY == 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        crate::setup::migration_review::OriginalOmission::validate_set(
            &self.omissions,
            source,
            self.caller.uid(),
        )?;
        Ok(())
    }
    #[cfg(feature = "capture-worker")]
    pub fn omissions(&self) -> &[crate::setup::migration_review::OriginalOmission] {
        &self.omissions
    }
    pub fn derive_applications_operation(
        operation: &str,
        binding: &str,
        identity: &[u8; 32],
    ) -> String {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"tilo-original-flatpak-operation-v1\0");
        digest.update(operation.as_bytes());
        digest.update([0]);
        digest.update(binding.as_bytes());
        digest.update([0]);
        digest.update(identity);
        let hash = digest.finalize();
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&hash[..16]);
        bytes[6] = (bytes[6] & 0x0f) | 0x80;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        uuid::Uuid::from_bytes(bytes).to_string()
    }
    pub(in crate::setup::home_capture) fn applications_operation(&self) -> io::Result<&str> {
        self.applications_operation
            .as_deref()
            .ok_or_else(|| failure("Original application operation unavailable"))
    }
    #[cfg(feature = "capture-worker")]
    pub(super) fn flatpak(
        &self,
    ) -> Option<&crate::setup::flatpak_transfer::offline_original::OriginalFlatpakClaim> {
        self.flatpak.as_ref()
    }
    pub fn required_protocol(&self) -> u32 {
        if self.is_applications_only() {
            super::APPLICATIONS_PROTOCOL
        } else {
            super::WORKER_PROTOCOL
        }
    }
    pub fn is_applications_only(&self) -> bool {
        matches!(self.material, OriginalMaterial::SelectedApplications)
    }
    pub fn validate_applications_only(
        &self,
        expected: Option<&crate::setup::flatpak_transfer::portable::Expectation>,
    ) -> io::Result<()> {
        self.validate_shape(&self.operation)?;
        if !self.is_applications_only() || self.flatpak.is_none() {
            return Err(failure(
                "Applications-only command lacks authenticated source consent",
            ));
        }
        if let Some(expected) = expected {
            expected.validate()?;
            if expected.consumer_protocol != 2
                || expected.operation != self.applications_operation()?
                || expected.setup_binding != self.setup_binding
                || expected
                    .limits
                    .sealed_bound()
                    .is_none_or(|bytes| bytes > self.sealed_reservation)
                || expected.sealed_bytes > self.sealed_reservation
            {
                return Err(failure(
                    "Applications-only artifact binding or budget differs",
                ));
            }
        }
        Ok(())
    }
    pub(super) fn validate_applications(
        &self,
        expected: Option<&crate::setup::flatpak_transfer::portable::Expectation>,
        home: &Expectation,
    ) -> io::Result<()> {
        if self.flatpak.is_some() != expected.is_some() {
            return Err(failure(
                "Selected original applications missing or invented",
            ));
        }
        if let Some(expected) = expected {
            expected.validate()?;
            let maximum = home.limits.sealed_bound().and_then(|home| {
                expected
                    .limits
                    .sealed_bound()
                    .and_then(|apps| home.checked_add(apps))
            });
            if maximum.is_none_or(|bytes| bytes > self.sealed_reservation)
                || expected.operation != self.applications_operation()?
                || expected.setup_binding != self.setup_binding
                || home
                    .sealed_bytes
                    .checked_add(expected.sealed_bytes)
                    .is_none_or(|bytes| bytes > self.sealed_reservation)
            {
                return Err(failure(
                    "Original selected package bindings or aggregate bounds differ",
                ));
            }
        }
        Ok(())
    }
    pub(super) fn export_operation(&self) -> &str {
        &self.export_operation
    }
    pub(super) fn setup_binding(&self) -> &str {
        &self.setup_binding
    }
    #[cfg(feature = "capture-worker")]
    pub(super) fn sealed_reservation(&self) -> u64 {
        self.sealed_reservation
    }
    /// Same selection/operation/budget checks used by the signed verifier.
    /// Claim bytes alone do not admit a source or an output target.
    pub(in crate::setup::home_capture) fn validate_installed_output(
        &self,
        home: Option<&Expectation>,
        applications: Option<&crate::setup::flatpak_transfer::portable::Expectation>,
    ) -> io::Result<()> {
        if self.is_applications_only() {
            if home.is_some() || applications.is_none() {
                return Err(failure("Installed applications-only output shape differs"));
            }
            self.validate_applications_only(applications)
        } else {
            let home = home.ok_or_else(|| failure("Installed Home output missing"))?;
            self.validate_export(&self.export_operation, &self.setup_binding, Some(home))?;
            self.validate_applications(applications, home)
        }
    }
    pub(super) fn validate_export(
        &self,
        operation: &str,
        binding: &str,
        expected: Option<&Expectation>,
    ) -> io::Result<()> {
        self.validate_shape(&self.operation)?;
        if self.is_applications_only() {
            return Err(failure("Applications-only consent cannot export Home"));
        }
        if operation != self.export_operation || binding != self.setup_binding {
            return Err(failure(
                "Original export differs from root-admitted declaration",
            ));
        }
        if let Some(expected) = expected {
            expected.validate()?;
            if expected.operation != operation
                || expected.setup_binding != binding
                || expected.sealed_bytes > self.sealed_reservation
                || expected
                    .limits
                    .sealed_bound()
                    .is_none_or(|bytes| bytes > self.sealed_reservation)
            {
                return Err(failure(
                    "Original package exceeds authenticated reservation or binding",
                ));
            }
        }
        Ok(())
    }
    /// This check observes credentials after the signed namespace shim dropped
    /// them. Claims never call setuid/setgid/setgroups or derive target NSS.
    #[cfg(feature = "capture-worker")]
    pub fn validate_reader(
        &self,
        source: &File,
    ) -> io::Result<Option<Arc<launcher::owned_service::VerifiedOriginalMountedRoots>>> {
        self.validate_shape(&self.operation)?;
        launcher::owned_service::verify_original_read_claim(self.binding_sha256()?)?;
        self.validate_source(source)?;
        let uid = rustix::process::getuid().as_raw();
        if uid == 0
            || rustix::process::geteuid().as_raw() != uid
            || uid != self.caller.uid()
            || rustix::process::getgid().as_raw() != self.caller.primary_gid()
            || rustix::process::getegid().as_raw() != self.caller.primary_gid()
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        if !(0..=65_536).contains(&count) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mut groups = vec![0; usize::try_from(count).map_err(io::Error::other)?];
        if unsafe { libc::getgroups(count, groups.as_mut_ptr()) } != count {
            return Err(io::Error::last_os_error());
        }
        groups.sort_unstable();
        groups.dedup();
        if groups != self.caller.supplementary_gids() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        launcher::owned_service::verify_original_mounted_roots(self.mounted_binding()?.as_ref())
            .map(|roots| roots.map(Arc::new))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn apps_only_phase_refuses_old_worker_and_home_commands_before_source_open() {
        let operation = uuid::Uuid::new_v4().to_string();
        let identity = [7u8; 32];
        let applications_operation =
            OriginalReadClaim::derive_applications_operation(&operation, "bound-setup", &identity);
        let claim = serde_json::json!({
            "schema":3,"material":"selected-applications","operation":operation,"setup_binding":"bound-setup",
            "export_operation":operation,"sealed_reservation":1048576,"omissions":[],
            "identity_public_key":identity,"source_plan_sha256":(vec![8u8;32]),
            "declaration_sha256":(vec![9u8;32]),"source":[9,10,1000],
            "caller":{"schema":1,"uid":1000,"primary_gid":1001,"supplementary_gids":[1002]},
            "applications_operation":applications_operation,
            "flatpak":{"schema":1,"operation":operation,"original_uid":1000,
                "inventory_sha256":(vec![10u8;32]),"inventory_len":100,
                "projections":[{"index":0,"device":9,"inode":12,"owner":1000}],
                "selected_data_ids":["so.example.Selected"]}
        });
        let wire = serde_json::json!({"schema":6,"operation":operation,"custody":null,
            "command":{"action":"review-original","authority":claim}});
        let request: Request = serde_json::from_value(wire.clone()).unwrap();
        request.validate_protocol().unwrap();
        let exported = request
            .into_export(Zeroizing::new(b"synthetic password".to_vec()))
            .unwrap();
        assert!(matches!(
            exported.command,
            Command::ExportOriginalApplications { .. }
        ));
        let mut old = wire.clone();
        old["schema"] = serde_json::json!(5);
        let old: Request = serde_json::from_value(old).unwrap();
        assert_eq!(
            old.validate_protocol().unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        let mut wrong = wire.clone();
        wrong["command"]["action"] = serde_json::json!("export-original");
        wrong["command"]["export_operation"] = serde_json::json!(operation);
        wrong["command"]["setup_binding"] = serde_json::json!("bound-setup");
        wrong["command"]["password"] = serde_json::json!(b"synthetic password".to_vec());
        let wrong: Request = serde_json::from_value(wrong).unwrap();
        assert!(wrong.validate_protocol().is_err());
        let baseline: OriginalReadClaim =
            serde_json::from_value(wire["command"]["authority"].clone()).unwrap();
        // This is the production pre-journal validator, using the same decoded
        // claim as the signed worker request above; it grants no source authority.
        let applications = crate::setup::flatpak_transfer::portable::Expectation {
            schema: 1,
            consumer_protocol: 2,
            operation: baseline.applications_operation().unwrap().into(),
            purpose: crate::setup::portable_transfer::Purpose::FlatpakApplications,
            filename: format!(
                "applications-{}.sealed",
                baseline.applications_operation().unwrap()
            ),
            setup_binding: "bound-setup".into(),
            sealed_bytes: 4096,
            plaintext_bytes: 1024,
            entries: 1,
            ciphertext_sha256: [2; 32],
            manifest_sha256: [3; 32],
            limits: crate::setup::portable_transfer::Limits {
                plaintext_bytes: 8192,
                entries: 16,
                metadata_bytes: 4096,
            },
        };
        baseline
            .validate_installed_output(None, Some(&applications))
            .unwrap();
        assert!(baseline.validate_installed_output(None, None).is_err());
        for altered in [
            {
                let mut value = applications.clone();
                value.consumer_protocol = 1;
                value
            },
            {
                let mut value = applications.clone();
                value.operation = uuid::Uuid::new_v4().to_string();
                value.filename = format!("applications-{}.sealed", value.operation);
                value
            },
            {
                let mut value = applications;
                value.setup_binding = "other-setup".into();
                value
            },
        ] {
            assert!(
                baseline
                    .validate_installed_output(None, Some(&altered))
                    .is_err()
            );
        }
        let mut changed = wire["command"]["authority"].clone();
        changed["flatpak"]["selected_data_ids"] = serde_json::json!(["so.example.Excluded"]);
        let changed: OriginalReadClaim = serde_json::from_value(changed).unwrap();
        assert_ne!(
            baseline.binding_sha256().unwrap(),
            changed.binding_sha256().unwrap()
        );
        let mut downgraded = wire["command"]["authority"].clone();
        downgraded["material"] = serde_json::json!("home");
        assert!(serde_json::from_value::<OriginalReadClaim>(downgraded).is_err());
    }
    #[test]
    fn original_request_digest_binds_authenticated_declaration_and_caller() {
        let mut value = serde_json::json!({
            "schema":2,"operation":uuid::Uuid::new_v4().to_string(),"setup_binding":"bound-setup",
            "export_operation":"PLACEHOLDER","sealed_reservation":1048576,"omissions":[],
            "identity_public_key":(vec![7u8;32]),"source_plan_sha256":(vec![8u8;32]),
            "declaration_sha256":(vec![9u8;32]),"source":[9,10,1000],
            "caller":{"schema":1,"uid":1000,"primary_gid":1001,"supplementary_gids":[1002]}
        });
        value["export_operation"] = value["operation"].clone();
        let claim: OriginalReadClaim = serde_json::from_value(value.clone()).unwrap();
        let digest = claim.binding_sha256().unwrap();
        // The declaration covers the authenticated SourceWitness, selected
        // medium and reservation; substitutions cannot reuse the shim proof.
        for field in [
            "declaration_sha256",
            "source_plan_sha256",
            "identity_public_key",
        ] {
            let mut changed = value.clone();
            changed[field] = serde_json::json!([11; 32].to_vec());
            let changed: OriginalReadClaim = serde_json::from_value(changed).unwrap();
            assert_ne!(changed.binding_sha256().unwrap(), digest);
        }
        let mut changed = value.clone();
        changed["caller"]["supplementary_gids"] = serde_json::json!([1003]);
        let changed: OriginalReadClaim = serde_json::from_value(changed).unwrap();
        assert_ne!(changed.binding_sha256().unwrap(), digest);
        let mut missing = value.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("declaration_sha256");
        assert!(serde_json::from_value::<OriginalReadClaim>(missing).is_err());
        let mut invalid = value;
        invalid["declaration_sha256"] = serde_json::json!([0; 32].to_vec());
        assert!(serde_json::from_value::<OriginalReadClaim>(invalid).is_err());
    }
    #[test]
    fn writable_original_projection_refuses_before_inventory_and_preserves_source() {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("document"), b"unchanged-source").unwrap();
        let held = File::open(source.path()).unwrap();
        let facts = held.metadata().unwrap();
        // Non-root UID is a source-owner claim, never a credential change.
        let owner = facts.uid();
        let mut value = serde_json::json!({
            "schema":2,"operation":uuid::Uuid::new_v4().to_string(),"setup_binding":"bound-setup",
            "export_operation":"PLACEHOLDER","sealed_reservation":1048576,"omissions":[],
            "identity_public_key":(vec![7u8;32]),"source_plan_sha256":(vec![8u8;32]),"declaration_sha256":(vec![9u8;32]),
            "source":[facts.dev(),facts.ino(),owner],
            "caller":{"schema":1,"uid":owner,"primary_gid":facts.gid(),"supplementary_gids":[]}
        });
        value["export_operation"] = value["operation"].clone();
        let claim: OriginalReadClaim = serde_json::from_value(value).unwrap();
        assert_eq!(
            claim.validate_source(&held).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::read(source.path().join("document")).unwrap(),
            b"unchanged-source"
        );
        assert_eq!(std::fs::read_dir(source.path()).unwrap().count(), 1);
        let other = tempfile::tempdir().unwrap();
        assert!(
            claim
                .validate_source(&File::open(other.path()).unwrap())
                .is_err()
        );
    }
    fn original_review() -> Request {
        let operation = uuid::Uuid::new_v4().to_string();
        serde_json::from_value(serde_json::json!({
            "schema":WORKER_PROTOCOL,"operation":operation,"custody":null,
            "command":{"action":"review-original","authority":{
                "schema":2,"operation":operation,"export_operation":operation,
                "sealed_reservation":1048576,"omissions":[],"setup_binding":"bound-setup",
                "identity_public_key":vec![7u8;32],"source_plan_sha256":vec![8u8;32],
                "declaration_sha256":vec![9u8;32],"source":[9,10,1000],
                "caller":{"schema":1,"uid":1000,"primary_gid":1001,"supplementary_gids":[1002]}
            }}
        }))
        .unwrap()
    }
    #[test]
    fn original_claim_wire_rejects_invalid_shape_and_preserves_home_defaults() {
        let request = serde_json::to_value(original_review()).unwrap();
        let wire = request["command"]["authority"].clone();
        let claim: OriginalReadClaim = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(claim).unwrap(), wire);
        for (field, invalid) in [
            ("schema", serde_json::json!(3)),
            ("operation", serde_json::json!("not-an-operation")),
            (
                "export_operation",
                serde_json::json!(uuid::Uuid::new_v4().to_string()),
            ),
            ("setup_binding", serde_json::json!("")),
            ("setup_binding", serde_json::json!("binding\n")),
            ("identity_public_key", serde_json::json!(vec![0u8; 32])),
            ("source_plan_sha256", serde_json::json!(vec![0u8; 32])),
            ("declaration_sha256", serde_json::json!(vec![0u8; 32])),
            ("sealed_reservation", serde_json::json!(0)),
            (
                "applications_operation",
                serde_json::json!(uuid::Uuid::new_v4().to_string()),
            ),
        ] {
            let mut altered = wire.clone();
            altered[field] = invalid;
            assert!(
                serde_json::from_value::<OriginalReadClaim>(altered).is_err(),
                "{field}"
            );
        }
        let mut wrong_owner = wire;
        wrong_owner["source"][2] = serde_json::json!(1003);
        assert!(serde_json::from_value::<OriginalReadClaim>(wrong_owner).is_err());
    }
    #[test]
    fn mounted_claim_wire_rejects_missing_digest_and_invalid_witness() {
        let wire = serde_json::json!({
            "witness": {
                "schema": 1,
                "primary": {
                    "schema": 1,
                    "operation": uuid::Uuid::new_v4().to_string(),
                    "namespace": crate::setup::migration_store::NativePath::encode(
                        std::path::Path::new("tilo-imager/home-capture/records")),
                    "record_sha256": vec![1u8; 32],
                    "plan_sha256": vec![2u8; 32]
                },
                "sha256": vec![3u8; 32]
            },
            "table_sha256": vec![4u8; 32]
        });
        let claim: OriginalMountedClaim = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(claim).unwrap(), wire);
        let mut missing_table = wire.clone();
        missing_table["table_sha256"] = serde_json::json!(vec![0u8; 32]);
        assert!(serde_json::from_value::<OriginalMountedClaim>(missing_table).is_err());
        let mut missing_witness_digest = wire.clone();
        missing_witness_digest["witness"]["sha256"] = serde_json::json!(vec![0u8; 32]);
        assert!(serde_json::from_value::<OriginalMountedClaim>(missing_witness_digest).is_err());
        let mut wrong_schema = wire.clone();
        wrong_schema["witness"]["schema"] = serde_json::json!(2);
        assert!(serde_json::from_value::<OriginalMountedClaim>(wrong_schema).is_err());
        let mut wrong_operation = wire;
        wrong_operation["witness"]["primary"]["operation"] = serde_json::json!("invalid");
        assert!(serde_json::from_value::<OriginalMountedClaim>(wrong_operation).is_err());
    }
    #[test]
    fn original_export_transform_binds_declared_operation_and_rejects_ordinary_route() {
        let request = original_review()
            .into_export(Zeroizing::new(b"synthetic-only".to_vec()))
            .unwrap();
        request.validate_protocol().unwrap();
        assert_eq!(
            request.require_capture_route().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire["command"]["action"], "export-original");
        assert_eq!(wire["command"]["export_operation"], wire["operation"]);
        assert_eq!(wire["command"]["setup_binding"], "bound-setup");
        for field in ["setup_binding", "export_operation"] {
            let mut altered = wire.clone();
            altered["command"][field] = serde_json::json!(uuid::Uuid::new_v4().to_string());
            let altered: Request = serde_json::from_value(altered).unwrap();
            assert!(altered.validate_protocol().is_err());
        }
        let mut unknown = wire.clone();
        unknown["command"]["output_path"] = serde_json::json!("/unapproved");
        assert!(serde_json::from_value::<Request>(unknown).is_err());
        let ordinary = Request {
            schema: WORKER_PROTOCOL,
            operation: uuid::Uuid::new_v4().to_string(),
            custody: None,
            command: Command::Review,
        };
        assert!(
            ordinary
                .into_export(Zeroizing::new(b"synthetic-only".to_vec()))
                .is_err()
        );
        assert!(
            original_review()
                .into_export(Zeroizing::new(Vec::new()))
                .is_err()
        );
        let mut custody = wire;
        custody["custody"] = serde_json::json!({"relative":{"platform":"Unix","units":b".local/state/tilo/installer/preparations".as_slice()},"stable":[1,16832,1000,1001]});
        let custody: Request = serde_json::from_value(custody).unwrap();
        assert!(custody.validate_protocol().is_err());
    }
    #[test]
    fn original_validate_refuses_cross_setup_and_package_over_authenticated_budget() {
        let review_wire = serde_json::to_value(original_review()).unwrap();
        let review = || serde_json::from_value::<Request>(review_wire.clone()).unwrap();
        let request = review();
        let expected = Expectation {
            schema: 1,
            operation: request.operation.clone(),
            purpose: crate::setup::portable_transfer::Purpose::Home,
            filename: format!("home-{}.sealed", request.operation),
            setup_binding: "bound-setup".into(),
            sealed_bytes: 4096,
            plaintext_bytes: 1024,
            entries: 1,
            runtime_sockets: 0,
            ciphertext_sha256: [1; 32],
            limits: crate::setup::portable_transfer::Limits {
                plaintext_bytes: 8192,
                entries: 16,
                metadata_bytes: 4096,
            },
        };
        let request = request
            .into_validate(expected.clone(), Zeroizing::new(b"synthetic-only".to_vec()))
            .unwrap();
        request.validate_protocol().unwrap();
        assert!(request.require_capture_route().is_err());
        let mut cross = expected.clone();
        cross.setup_binding = "another-setup".into();
        assert!(
            review()
                .into_validate(cross, Zeroizing::new(b"synthetic-only".to_vec()))
                .is_err()
        );
        let mut oversized = expected;
        oversized.limits.plaintext_bytes = 2 * 1024 * 1024;
        assert!(
            review()
                .into_validate(oversized, Zeroizing::new(b"synthetic-only".to_vec()))
                .is_err()
        );
    }
    #[test]
    fn selected_applications_are_bound_to_original_worker_and_cannot_be_omitted() {
        let mut wire = serde_json::to_value(original_review()).unwrap();
        wire["command"]["authority"]["flatpak"] = serde_json::json!({
            "schema":1,"operation":wire["operation"].clone(),"original_uid":1000,
            "inventory_sha256":vec![17u8;32],"inventory_len":8*1024*1024,
            "projections":[{"index":0,"device":21,"inode":22,"owner":0}]
        });
        wire["command"]["authority"]["applications_operation"] =
            serde_json::json!(OriginalReadClaim::derive_applications_operation(
                wire["operation"].as_str().unwrap(),
                "bound-setup",
                &[7u8; 32]
            ));
        let review = || serde_json::from_value::<Request>(wire.clone()).unwrap();
        let request = review();
        request.validate_protocol().unwrap();
        let encoded = serde_json::to_vec(&request).unwrap();
        // A full8MiB inventory is referenced and authenticated, never stuffed
        // into the64KiB worker frame or truncated to fit that frame.
        let mut frame = Vec::new();
        write_frame(&mut frame, &encoded, REQUEST_BOUND).unwrap();
        let decoded: Request =
            serde_json::from_slice(&read_frame(&mut frame.as_slice(), REQUEST_BOUND).unwrap())
                .unwrap();
        decoded.validate_protocol().unwrap();
        assert!(decoded.require_capture_route().is_err());
        let before = decoded
            .command
            .original_authority()
            .unwrap()
            .binding_sha256()
            .unwrap();
        for field in ["inventory_sha256", "inventory_len", "projections"] {
            let mut altered = wire.clone();
            match field {
                "inventory_sha256" => {
                    altered["command"]["authority"]["flatpak"][field] =
                        serde_json::json!(vec![18u8; 32])
                }
                "inventory_len" => {
                    altered["command"]["authority"]["flatpak"][field] = serde_json::json!(1048576)
                }
                _ => {
                    altered["command"]["authority"]["flatpak"][field][0]["inode"] =
                        serde_json::json!(23)
                }
            }
            let altered: Request = serde_json::from_value(altered).unwrap();
            assert_ne!(
                altered
                    .command
                    .original_authority()
                    .unwrap()
                    .binding_sha256()
                    .unwrap(),
                before
            );
        }
        let home = Expectation {
            schema: 1,
            operation: request.operation.clone(),
            purpose: crate::setup::portable_transfer::Purpose::Home,
            filename: format!("home-{}.sealed", request.operation),
            setup_binding: "bound-setup".into(),
            sealed_bytes: 4096,
            plaintext_bytes: 1024,
            entries: 1,
            runtime_sockets: 0,
            ciphertext_sha256: [1; 32],
            limits: crate::setup::portable_transfer::Limits {
                plaintext_bytes: 8192,
                entries: 16,
                metadata_bytes: 4096,
            },
        };
        assert!(
            review()
                .into_validate(home.clone(), Zeroizing::new(b"synthetic-only".to_vec()))
                .is_err()
        );
        let apps = crate::setup::flatpak_transfer::portable::Expectation {
            schema: 1,
            consumer_protocol: 1,
            operation: request
                .command
                .original_authority()
                .unwrap()
                .applications_operation()
                .unwrap()
                .into(),
            purpose: crate::setup::portable_transfer::Purpose::FlatpakApplications,
            filename: format!(
                "applications-{}.sealed",
                request
                    .command
                    .original_authority()
                    .unwrap()
                    .applications_operation()
                    .unwrap()
            ),
            setup_binding: "bound-setup".into(),
            sealed_bytes: 4096,
            plaintext_bytes: 1024,
            entries: 1,
            ciphertext_sha256: [2; 32],
            manifest_sha256: [3; 32],
            limits: home.limits,
        };
        let selected = review()
            .into_validate_selected(
                home.clone(),
                Some(apps.clone()),
                Zeroizing::new(b"synthetic-only".to_vec()),
            )
            .unwrap();
        selected.validate_protocol().unwrap();
        let mut changed = apps.clone();
        changed.setup_binding = "other".into();
        assert!(
            review()
                .into_validate_selected(
                    home.clone(),
                    Some(changed),
                    Zeroizing::new(b"synthetic-only".to_vec())
                )
                .is_err()
        );
        let mut absent = wire.clone();
        absent["command"]["authority"]
            .as_object_mut()
            .unwrap()
            .remove("flatpak");
        absent["command"]["authority"]
            .as_object_mut()
            .unwrap()
            .remove("applications_operation");
        let absent: Request = serde_json::from_value(absent).unwrap();
        assert!(
            absent
                .into_validate_selected(
                    home.clone(),
                    Some(apps.clone()),
                    Zeroizing::new(b"synthetic-only".to_vec())
                )
                .is_err()
        );
        let mut too_small = wire.clone();
        too_small["command"]["authority"]["sealed_reservation"] = serde_json::json!(4096);
        let too_small: Request = serde_json::from_value(too_small).unwrap();
        assert!(
            too_small
                .into_validate_selected(
                    home,
                    Some(apps),
                    Zeroizing::new(b"synthetic-only".to_vec())
                )
                .is_err()
        );
    }
}
