//! Register the exact destination-staging operation, then amend only the existing
//! password-authenticated setup generation. No source ciphertext is fabricated.
use super::*;
use crate::setup::{
    migration_handoff::{HomePolicy, Plan},
    migration_intent::MigrationIntent,
};
impl PreparedMedia {
    pub(in crate::setup) fn native_prepare_staging_copy(
        self: &Arc<Self>,
        intent: &MigrationIntent,
        source: &Plan,
        reservation: u64,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        super::super::super::migration::check_cancel(cancel)?;
        if source.home_policy != HomePolicy::Copy
            || !(source.categories.home || source.categories.flatpak)
            || self.retained.as_ref() != Some(intent)
            || self.home.is_some()
            || self.applications.is_some()
            || self.manifest.portable_home.is_some()
            || self.manifest.offline_home.is_some()
            || self.manifest.offline_applications.is_some()
            || self.manifest.completed_home.is_some()
            || self.manifest.completed_applications_only
            || self.manifest.retained_caller.is_some()
            || self.manifest.planned_btrfs_resize.is_some()
            || matches!(
                self.manifest.identity_kind,
                prepared_setup::IdentityKind::CurrentDevice { .. }
            )
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Original copy differs from retained staging selection",
            ));
        }
        let mut generation = self
            .transfer_generation
            .lock()
            .map_err(|_| io::Error::other("Staging generation custody unavailable"))?;
        let (expected, _) = source.original_inventory_selection()?;
        if let Some(copy) = &self.manifest.original_copy {
            if copy.source() != &expected || copy.reservation() != reservation {
                return Err(io::Error::other("Staging source declaration changed"));
            }
            self.validate_for_write(cancel)?;
            return Ok(self.clone());
        }
        if let Some(prepared) = &*generation {
            let copy = prepared
                .manifest
                .original_copy
                .as_ref()
                .ok_or_else(|| io::Error::other("Staging generation has another purpose"))?;
            if copy.source() != &expected || copy.reservation() != reservation {
                return Err(io::Error::other("Staging source declaration changed"));
            }
            prepared.validate_for_write(cancel)?;
            return Ok(prepared.clone());
        }
        if self.delivery_started.load(Ordering::Acquire) {
            return Err(io::Error::other("Prepared delivery already began"));
        }
        self.validate_for_write(cancel)?;
        let registered = intent.register_staging_copy(source, cancel)?;
        let declaration = prepared_setup::OriginalCopyDeclaration::new(
            registered,
            self.manifest.setup_binding.clone(),
            self.public_key,
            reservation,
        )?;
        let prepared = offline::reseal_staging_copy(self, declaration, cancel)?;
        intent.for_media()?;
        prepared.validate_for_write(cancel)?;
        super::super::super::migration::check_cancel(cancel)?;
        *generation = Some(prepared.clone());
        Ok(prepared)
    }
}

impl PreparedMedia {
    pub(in crate::setup) fn native_prepare_computer_applications(
        self: &Arc<Self>,
        intent: &MigrationIntent,
        operation: &str,
        installation: &crate::setup::staging::plan::Plan,
        reservation: u64,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        super::super::super::migration::check_cancel(cancel)?;
        if self.retained.as_ref() != Some(intent)
            || self.home.is_some()
            || self.applications.is_some()
            || self.manifest.original_copy.is_some()
            || self.manifest.portable_home.is_some()
            || self.manifest.offline_home.is_some()
            || self.manifest.offline_applications.is_some()
            || self.manifest.completed_home.is_some()
            || self.manifest.completed_applications_only
            || self.manifest.retained_caller.is_none()
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let apps = intent
            .computer_applications()?
            .ok_or_else(|| io::Error::other("Selected application grant missing"))?;
        let bound: Plan = serde_json::from_value(apps.bind(operation, installation)?)?;
        if let Some(composite) = &self.manifest.computer_applications {
            composite.matches_staging(&self.manifest, intent, operation, installation)?;
            let copy = self
                .manifest
                .installed_original_copy_declaration()?
                .ok_or_else(|| io::Error::other("Computer Apps declaration absent"))?;
            if copy.source() != &bound.original_inventory_selection()?.0
                || copy.reservation() != reservation
            {
                return Err(io::Error::other("Computer Apps staging changed"));
            }
            self.validate_for_write(cancel)?;
            return Ok(self.clone());
        }
        let mut generation = self
            .transfer_generation
            .lock()
            .map_err(|_| io::Error::other("Staging generation custody unavailable"))?;
        if let Some(prepared) = &*generation {
            prepared
                .manifest
                .computer_applications
                .as_ref()
                .ok_or_else(|| io::Error::other("Staging generation has another purpose"))?
                .matches_staging(&prepared.manifest, intent, operation, installation)?;
            let copy = prepared
                .manifest
                .installed_original_copy_declaration()?
                .ok_or_else(|| io::Error::other("Staging generation has another purpose"))?;
            if prepared.manifest.computer_applications.is_none()
                || copy.source() != &bound.original_inventory_selection()?.0
                || copy.reservation() != reservation
            {
                return Err(io::Error::other(
                    "Computer application staging selection changed",
                ));
            }
            prepared.validate_for_write(cancel)?;
            return Ok(prepared.clone());
        }
        if self.delivery_started.load(Ordering::Acquire) {
            return Err(io::Error::other("Prepared delivery already began"));
        }
        self.validate_for_write(cancel)?;
        let registered = apps.register_staging_copy(&bound, cancel)?;
        let copy = prepared_setup::OriginalCopyDeclaration::new(
            registered,
            self.manifest.setup_binding.clone(),
            self.public_key,
            reservation,
        )?;
        let composite = prepared_setup::SharedHomeApplicationsDeclaration::new(
            &self.manifest,
            intent,
            copy,
            installation,
        )?;
        let prepared = offline::reseal_computer_applications(self, composite, cancel)?;
        intent.for_media()?;
        prepared.validate_for_write(cancel)?;
        super::super::super::migration::check_cancel(cancel)?;
        *generation = Some(prepared.clone());
        Ok(prepared)
    }
}
