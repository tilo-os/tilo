//! Retained guided inputs, handed to the canonical gated Full-OS surface.
use super::{Destination, Imager, InstallerPreview, InstallerPreviewEffect, Page};
use crate::setup::{migration_intent::MigrationIntent, staging};
use imager_backend::prepared_setup::PublicInstallProfile;
use std::sync::Arc;
use ui::framework::SurfaceCx;
use ui::widget::{ButtonRole, Row};

pub(super) struct Direct {
    pub context: Option<SurfaceCx<Imager>>,
    pub pending: Option<Snapshot>,
    pub belongings_pending: bool,
    pub belongings_continue: bool,
    pub prepared_belongings: Option<Arc<super::prepared_media::PreparedMedia>>,
    pub active: bool,
    pub navigation: ui::LocalState<bool>,
}

impl Default for Direct {
    fn default() -> Self {
        Self {
            context: None,
            pending: None,
            belongings_pending: false,
            belongings_continue: false,
            prepared_belongings: None,
            active: false,
            navigation: ui::LocalState::new(false),
        }
    }
}

pub(super) struct Snapshot {
    facts: staging::probe::HostFacts,
    plan: staging::plan::Plan,
    profile: imager_backend::imager_model::InstallProfile,
    migration: Option<MigrationIntent>,
}

impl InstallerPreview {
    pub(super) fn public_install_profile(&self) -> Result<PublicInstallProfile, &'static str> {
        let mut profile = self.install_preferences.clone();
        // Linux GECOS is a presentation mapping, not the person's global name.
        profile.user.full_name = self
            .journey
            .name
            .chars()
            .filter(|character| !character.is_control() && *character != ':')
            .take(100)
            .collect();
        profile.device_label.clone_from(&self.journey.device_name);
        profile.purpose.clone_from(&self.journey.purpose);
        profile.user.password.clear();
        profile.encryption_passphrase.clear();
        profile.prepared_setup = None;
        PublicInstallProfile::from_draft(&profile)
            .map_err(|_| "installer-preview-install-profile-invalid")
    }

    pub(super) fn direct_available(&self) -> bool {
        self.live
            && self.journey.page == Page::Review
            && self.journey.destination == Some(Destination::Computer)
            && self.direct.context.is_some()
            && !self.media_preparation.busy()
            && !self.migration_busy()
            && !self.personal_busy()
            && self.personal.prepared_handle().is_some()
            && crate::setup::internal_install_flow::live_gate_open()
            && self
                .storage
                .facts
                .as_ref()
                .is_some_and(|facts| facts.offers_full_os())
            && self.storage.admit().is_ok()
            && self.recovery.evidence().is_ok()
    }

    pub(super) fn reviewed_belongings(
        &self,
    ) -> Result<
        (
            Option<MigrationIntent>,
            Option<super::accounts::PreparedAccounts>,
        ),
        &'static str,
    > {
        self.recovery
            .evidence()
            .map_err(|_| "installer-recovery-copy-required")?;
        let wants_migration = self.journey.migrate == Some(true)
            && (self.journey.categories[0] || self.selected_applications());
        let migration = if wants_migration {
            Some(
                self.migration_selection
                    .reviewed_intent()
                    .ok_or("installer-preview-import-unavailable")?,
            )
        } else {
            None
        };
        if self.journey.migrate == Some(true)
            && self.journey.categories[0]
            && self.selected_applications()
        {
            migration
                .as_ref()
                .ok_or("installer-preview-import-unavailable")?
                .validate_computer_applications_selection(&self.selected_portable_apps())
                .map_err(|_| "installer-preview-import-unavailable")?;
        }
        let accounts = if self.journey.migrate == Some(true)
            && self.journey.categories[2]
            && self.accounts.has_prepared_connections()
        {
            Some(
                self.accounts
                    .prepared_handle()
                    .ok_or("installer-preview-account-handoff-unavailable")?,
            )
        } else {
            None
        };
        Ok((migration, accounts))
    }

    /// Prepare the chosen identity and source grants without firmware checks,
    /// installation admission, a delegated installer or any disk write.
    pub(super) fn prepare_belongings(&mut self) -> Result<(), &'static str> {
        if !self.live
            || !matches!(
                self.journey.page,
                Page::Devices | Page::Belongings | Page::Accounts | Page::Review | Page::Storage
            )
            || self.journey.destination != Some(Destination::Computer)
            || self.media_preparation.busy()
            || self.personal_busy()
            || self.migration_busy()
            || self.direct.active
        {
            return Err("installer-preview-wrong-page");
        }
        let identity = self
            .personal
            .prepared_handle()
            .ok_or("installer-preview-identity-not-prepared")?;
        let (migration, accounts) = self.reviewed_belongings()?;
        let install_profile = self.public_install_profile()?;
        self.media_preparation.start_with_selections(
            identity,
            accounts,
            super::prepared_media::MediaSelections {
                sessions: self.belongings.sessions.selected_for_accounts(
                    self.journey.migrate == Some(true) && self.journey.categories[2],
                ),
                home_applications: false,
                home: None,
                retained: migration,
                // Source consent is prepared before target allocation. The
                // real staged application declaration remains a later review.
                planned_direct: None,
                applications: None,
                browser: (self.journey.migrate == Some(true)
                    && self.belongings.browser.available())
                .then(|| self.belongings.browser.selected())
                .filter(|selected| !selected.profiles().is_empty()),
            },
            zeroize::Zeroizing::new(self.journey.password.clone()),
            Some(install_profile),
        )?;
        self.direct.prepared_belongings = None;
        self.direct.belongings_pending = true;
        Ok(())
    }

    pub(super) fn prepare_direct(&mut self) -> Result<(), &'static str> {
        if !self.direct_available() {
            return Err("installer-preview-storage-unavailable");
        }
        let public = self.public_install_profile()?;
        let profile = public
            .to_draft()
            .map_err(|_| "installer-preview-install-profile-invalid")?;
        let identity = self
            .personal
            .prepared_handle()
            .ok_or("installer-preview-identity-not-prepared")?;
        let (migration, accounts) = self.reviewed_belongings()?;
        let snapshot = Snapshot {
            facts: self
                .storage
                .facts
                .clone()
                .ok_or("installer-preview-storage-unavailable")?,
            plan: self
                .storage
                .plan
                .as_ref()
                .and_then(|plan| plan.as_ref().ok())
                .cloned()
                .ok_or("installer-preview-storage-unavailable")?,
            profile,
            migration,
        };
        if snapshot.plan.shrink.is_none()
            && let Some(prepared) = self.direct.prepared_belongings.clone()
            && prepared.matches_reviewed_belongings(&identity, &public, snapshot.migration.as_ref())
        {
            self.direct.pending = Some(snapshot);
            return self.finish_direct(prepared);
        }
        self.media_preparation.start_with_selections(
            identity,
            accounts,
            super::prepared_media::MediaSelections {
                sessions: self.belongings.sessions.selected_for_accounts(
                    self.journey.migrate == Some(true) && self.journey.categories[2],
                ),
                home_applications: false,
                home: None,
                retained: snapshot.migration.clone(),
                planned_direct: Some(snapshot.plan.clone()),
                // The separately captured original Apps grant is carried by
                // retained and registered against the real staging operation.
                // This slot is the unrelated portal-selected portable export.
                applications: None,
                browser: (self.journey.migrate == Some(true)
                    && self.belongings.browser.available())
                .then(|| self.belongings.browser.selected())
                .filter(|selected| !selected.profiles().is_empty()),
            },
            zeroize::Zeroizing::new(self.journey.password.clone()),
            Some(public),
        )?;
        self.direct.pending = Some(snapshot);
        Ok(())
    }

    pub(super) fn finish_direct(
        &mut self,
        prepared: Arc<super::prepared_media::PreparedMedia>,
    ) -> Result<(), &'static str> {
        let mut snapshot = self
            .direct
            .pending
            .take()
            .ok_or("installer-preview-storage-unavailable")?;
        prepared
            .bind_recovery(
                self.recovery
                    .evidence()
                    .map_err(|_| "installer-recovery-copy-required")?,
                &snapshot.plan,
            )
            .map_err(|_| "installer-recovery-write-guard")?;
        snapshot.profile.prepared_setup = Some(prepared.expectation().clone());
        let child =
            crate::setup::view::SetupApp::from_guided_review(crate::setup::view::ReviewedFullOs {
                facts: snapshot.facts,
                plan: snapshot.plan,
                profile: snapshot.profile,
                migration: snapshot.migration,
                prepared,
            })
            .map_err(|_| "installer-preview-direct-review-unavailable")?;
        let navigation = child.guided_navigation_guard();
        let context = self
            .direct
            .context
            .as_ref()
            .ok_or("installer-preview-storage-unavailable")?;
        let child = context.surface(child)?;
        let back = self
            .button(
                "installer.back",
                "installer-preview-back",
                InstallerPreviewEffect::BackInPreview,
            )
            .role(ButtonRole::Plain)
            .enabled_from(&navigation);
        self.composed = Some(Box::new(
            crate::page::stack()
                .push(child)
                .push(Box::new(Row::new().push(Box::new(back)))),
        ));
        self.direct.navigation = navigation;
        self.direct.active = true;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_refuses_missing_review_without_discarding_personal_choices() {
        let mut preview = InstallerPreview {
            live: true,
            ..Default::default()
        };
        preview.journey.page = Page::Storage;
        preview.journey.destination = Some(Destination::Computer);
        preview.journey.name = "Chosen name".into();
        preview.journey.password = "private chosen password".into();
        assert_eq!(
            preview.prepare_direct(),
            Err("installer-preview-storage-unavailable")
        );
        assert!(preview.direct.pending.is_none());
        assert!(!preview.media_preparation.busy());
        assert_eq!(preview.journey.name, "Chosen name");
        assert_eq!(preview.journey.password, "private chosen password");
        assert!(!preview.direct.active);
    }

    #[test]
    fn chosen_device_name_survives_authenticated_and_media_profile_roundtrip() {
        let mut preview = InstallerPreview::default();
        preview.journey.name = "Ada".into();
        preview.journey.device_name = "Ada’s workstation 🌿".into();
        let public = preview.public_install_profile().unwrap();
        let restored: PublicInstallProfile =
            serde_json::from_slice(&serde_json::to_vec(&public).unwrap()).unwrap();
        let draft = restored.to_draft().unwrap();
        assert_eq!(draft.device_label, preview.journey.device_name);
        let media = crate::setup::staging::handoff::profile_from_install(&draft).unwrap();
        assert_eq!(media["device_label"], preview.journey.device_name);
        let mut legacy = serde_json::to_value(&restored).unwrap();
        legacy.as_object_mut().unwrap().remove("device_label");
        let legacy: PublicInstallProfile = serde_json::from_value(legacy).unwrap();
        assert!(legacy.to_draft().unwrap().device_label.is_empty());
        preview.journey.device_name = "é".repeat(32);
        assert_eq!(
            preview.public_install_profile().unwrap().device_label,
            preview.journey.device_name
        );
        preview.journey.device_name = "é".repeat(33);
        assert!(preview.public_install_profile().is_err());
        assert_eq!(preview.journey.device_name.len(), 66);
        preview.journey.device_name = "invalid\nname".into();
        assert!(preview.public_install_profile().is_err());
    }

    #[test]
    fn authenticated_install_preferences_use_chosen_name_without_passwords() {
        let mut preview = InstallerPreview::default();
        preview.journey.name = "Ada Lovelace".into();
        preview.install_preferences.user.password = "must not travel".into();
        preview.install_preferences.encryption_passphrase = "also private".into();
        let profile = preview.public_install_profile().unwrap();
        assert_eq!(profile.full_name, "Ada Lovelace");
        let bytes = serde_json::to_string(&profile).unwrap();
        assert!(!bytes.contains("must not travel"));
        assert!(!bytes.contains("also private"));
        preview.journey.name = format!("Name: {}", "界".repeat(120));
        let public = preview.public_install_profile().unwrap();
        assert!(!public.full_name.contains(':'));
        assert_eq!(public.full_name.chars().count(), 100);
        assert!(preview.journey.name.contains(':'));
        assert!(preview.journey.name.chars().count() > 100);
        preview.install_preferences.user.username = "root".into();
        assert!(preview.public_install_profile().is_err());
    }
}
