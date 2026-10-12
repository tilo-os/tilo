//! Separate original applications producer. No Home source or archive is minted.
use super::*;
impl PreparedMedia {
    /// Only the retained selected app intent enters this phase. Root registration
    /// supplies original caller/witness; selected medium supplies export budget.
    pub(in crate::setup) fn prepare_applications_offline(
        self: &Arc<Self>,
        media: &Arc<OfflineMediaReview>,
        intent: &super::super::super::super::migration_intent::MigrationIntent,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        super::super::super::super::migration::check_cancel(cancel)?;
        let mut generation = self
            .transfer_generation
            .lock()
            .map_err(|_| io::Error::other("Preparation generation unavailable"))?;
        if self.delivery_started.load(Ordering::Acquire) || generation.is_some() {
            return Err(io::Error::other("Preparation generation is already frozen"));
        }
        media.validate()?;
        let plan = intent.for_pending_applications()?;
        if matches!(
            self.manifest.identity_kind,
            prepared_setup::IdentityKind::CurrentDevice { .. }
        ) || self.home.is_some()
            || self.retained.is_some()
            || self.applications.is_some()
            || self.manifest.portable_home.is_some()
            || self.manifest.completed_home.is_some()
            || self.manifest.portable_flatpak.is_some()
            || (!plan.categories.accounts
                && (!self.account_ids.is_empty() || self.manifest.service_sessions.is_some()))
        {
            return Err(io::Error::other(
                "Applications-only pending selection differs",
            ));
        }
        let refreshed;
        let selected = if self.browser.is_some() && !self.browser_frozen {
            refreshed = self.refresh_browser_uncached(&password, cancel)?;
            &*refreshed
        } else {
            &**self
        };
        if let Some(browser) = &selected.browser {
            for (source, _) in browser.source.profiles() {
                source.validate()?;
            }
            browser
                .archive
                .lock()
                .map_err(|_| io::Error::other("Browser custody unavailable"))?
                .verify_held_ciphertext(cancel)?;
        }
        let source_geometry = original_source_plans(&plan)?;
        media.bind_source_geometry(source_geometry)?;
        let reservation = original_budget(selected, media)?;
        let registered = intent.register_pending_applications(cancel)?;
        let (protected_plan, _) = plan.original_inventory_selection()?;
        if registered.plan != protected_plan {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mut declaration = prepared_setup::OfflineApplicationsPlan::new(
            registered.plan,
            self.manifest.setup_binding.clone(),
            self.public_key,
            media.layout.planned_binding()?,
            registered.original_caller,
            reservation,
        )?
        .with_source_witness(registered.witness)?;
        if let Some(mounted) = registered.mounted {
            declaration = declaration.with_mounted_witness(mounted)?;
        }
        let prepared = reseal_selection(
            selected,
            prepared_setup::OriginalSelectionPlan::Applications(declaration),
            &password,
            cancel,
            Some(intent.clone()),
        )?;
        media
            .layout
            .require_capacity(prepared.reserved_data_bytes.load(Ordering::Acquire))?;
        media.validate()?;
        if intent.for_pending_applications()? != plan {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        super::super::super::super::migration::check_cancel(cancel)?;
        *generation = Some(Arc::clone(&prepared));
        Ok(prepared)
    }
}
