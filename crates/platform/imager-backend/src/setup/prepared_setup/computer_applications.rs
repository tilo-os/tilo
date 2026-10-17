//! Authenticated pending composition. It retains two independent declarations;
//! the scope below describes staging, never completed application import.
use super::{Manifest, OriginalCopyDeclaration, invalid};
use crate::setup::{migration_handoff::HomePolicy, migration_intent::MigrationIntent, staging};
use serde::{Deserialize, Serialize};
use std::io;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedHomeApplicationsDeclaration {
    schema: u32,
    source_operation: String,
    applications_review_operation: String,
    staging_operation: String,
    staging_plan: staging::plan::Plan,
    applications: OriginalCopyDeclaration,
}
#[derive(Clone, Debug, Serialize)]
pub struct ComputerApplicationsScope {
    schema: u32,
    source_operation: String,
    applications_operation: String,
    staging_operation: String,
    staging_plan_sha256: String,
    setup_binding: String,
    identity_public_key: [u8; 32],
}
fn operation(value: &str) -> io::Result<()> {
    let id = uuid::Uuid::parse_str(value).map_err(|_| invalid())?;
    if id.is_nil() || id.to_string() != value {
        return Err(invalid());
    }
    Ok(())
}
impl SharedHomeApplicationsDeclaration {
    pub fn new(
        manifest: &Manifest,
        main: &MigrationIntent,
        applications: OriginalCopyDeclaration,
        staging_plan: &staging::plan::Plan,
    ) -> io::Result<Self> {
        let apps = main.computer_applications()?.ok_or_else(invalid)?;
        let original = main.for_media()?;
        if manifest
            .retained_home
            .as_ref()
            .ok_or_else(invalid)?
            .as_plan()
            != &original
        {
            return Err(invalid());
        }
        apps.validate_staging_copy(applications.source())?;
        let value = Self {
            schema: 1,
            source_operation: original.operation_id,
            applications_review_operation: apps.source_plan().operation_id,
            staging_operation: applications.source().operation_id.clone(),
            staging_plan: staging_plan.clone(),
            applications,
        };
        let mut amended = manifest.clone();
        amended.planned_btrfs_resize = value.planned_resize(manifest)?;
        value.validate(&amended)?;
        Ok(value)
    }
    pub fn planned_resize(
        &self,
        manifest: &Manifest,
    ) -> io::Result<Option<super::PlannedBtrfsResize>> {
        let resize = super::PlannedBtrfsResize::from_review(
            manifest.retained_home.as_ref().ok_or_else(invalid)?,
            &self.staging_plan,
        )?;
        // The installed physical companion verifies the actually shrunk shared
        // filesystem and maps its original inventory roots. A different resized
        // source needs a separately admitted completion proof, never omission.
        if self.staging_plan.shrink.is_some() && resize.is_none() {
            return Err(invalid());
        }
        Ok(resize)
    }
    pub fn matches_staging(
        &self,
        manifest: &Manifest,
        main: &MigrationIntent,
        operation: &str,
        installation: &staging::plan::Plan,
    ) -> io::Result<()> {
        self.validate(manifest)?;
        let apps = main.computer_applications()?.ok_or_else(invalid)?;
        if self.staging_operation != operation
            || self.staging_plan != *installation
            || self.source_operation != main.source_plan().operation_id
            || self.applications_review_operation != apps.source_plan().operation_id
            || manifest
                .retained_home
                .as_ref()
                .ok_or_else(invalid)?
                .as_plan()
                != &main.for_media()?
        {
            return Err(invalid());
        }
        apps.validate_staging_copy(self.applications.source())
    }
    pub(super) fn validate(&self, manifest: &Manifest) -> io::Result<()> {
        let main = manifest.retained_home.as_ref().ok_or_else(invalid)?;
        main.validate()?;
        let main = main.as_plan();
        let caller = manifest.retained_caller.as_ref().ok_or_else(invalid)?;
        caller.validate_binding(main, &manifest.setup_binding, &manifest.identity_public_key)?;
        let copy = &self.applications;
        copy.validate()?;
        let apps = copy.source();
        for id in [
            &self.source_operation,
            &self.applications_review_operation,
            &self.staging_operation,
        ] {
            operation(id)?;
        }
        main.validate_for_install(&self.staging_plan)?;
        // OriginalCopy holds a protected inventory marker. Its real producer
        // validated every held inline root before registration; the consumer
        // reopens that exact sidecar. Geometry parsing must not reinterpret the
        // marker as an inline inventory or discard its witness.
        let mut physical = apps.clone();
        physical.flatpak_inventory = None;
        physical.validate_for_install(&self.staging_plan)?;
        let resize = self.planned_resize(manifest)?;
        if self.schema != 1
            || self.source_operation != main.operation_id
            || self.source_operation == self.applications_review_operation
            || self.source_operation == self.staging_operation
            || self.applications_review_operation == self.staging_operation
            || self.staging_operation != apps.operation_id
            || !main.categories.home
            || main.categories.flatpak
            || main.home_policy != HomePolicy::AccessReadWrite
            || apps.categories.home
            || !apps.categories.flatpak
            || apps.categories.accounts
            || apps.home_policy != HomePolicy::Copy
            || main.home != apps.home
            || caller.requested_caller() != copy.requested_caller()
            || copy.setup_binding() != manifest.setup_binding
            || copy.identity_public_key() != manifest.identity_public_key
            || manifest.planned_btrfs_resize != resize
            || manifest.original_copy.is_some()
            || manifest.offline_home.is_some()
            || manifest.offline_applications.is_some()
            || manifest.portable_home.is_some()
            || manifest.completed_home.is_some()
            || manifest.completed_applications_only
            || manifest.portable_flatpak.is_some()
            || matches!(
                manifest.identity_kind,
                super::IdentityKind::CurrentDevice { .. }
            )
        {
            return Err(invalid());
        }
        Ok(())
    }
}
impl Manifest {
    pub fn installed_original_copy_declaration(
        &self,
    ) -> io::Result<Option<&OriginalCopyDeclaration>> {
        self.validate()?;
        Ok(match &self.computer_applications {
            Some(composite) => Some(&composite.applications),
            None => self.original_copy.as_ref(),
        })
    }
    pub fn pending_computer_applications_scope(
        &self,
    ) -> io::Result<Option<ComputerApplicationsScope>> {
        self.validate()?;
        let Some(composite) = &self.computer_applications else {
            return Ok(None);
        };
        Ok(Some(ComputerApplicationsScope {
            schema: 1,
            source_operation: composite.source_operation.clone(),
            applications_operation:
                crate::setup::home_capture::OriginalReadClaim::derive_applications_operation(
                    &composite.staging_operation,
                    &self.setup_binding,
                    &self.identity_public_key,
                ),
            staging_operation: composite.staging_operation.clone(),
            staging_plan_sha256: composite.staging_plan.digest().map_err(io::Error::other)?,
            setup_binding: self.setup_binding.clone(),
            identity_public_key: self.identity_public_key,
        }))
    }
}
