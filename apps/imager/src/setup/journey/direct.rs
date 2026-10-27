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
    /// Sensitive work finishes on the destination after its first-boot login
    /// password exists; no ciphertext or preparation password crosses the
    /// installation.
    deferred: bool,
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
        self.direct_blocker().is_none()
    }

    /// The explanation and the action share admission; display never grants permission.
    pub(super) fn direct_blocker(&self) -> Option<String> {
        use crate::setup::strings::t;
        let key = if !self.live
            || self.journey.page != Page::Review
            || self.journey.destination != Some(Destination::Computer)
        {
            "installer-review-not-ready"
        } else if self.direct.context.is_none() {
            "installer-review-context-pending"
        } else if self.media_preparation.busy() || self.migration_busy() || self.personal_busy() {
            "installer-review-preparation-pending"
        } else if self.personal.prepared_handle().is_none() {
            "installer-review-identity-pending"
        } else if !crate::setup::internal_install_flow::live_gate_open() {
            "installer-preview-storage-release-blocked"
        } else {
            return self.direct_storage_blocker();
        };
        Some(t(key))
    }

    fn direct_storage_blocker(&self) -> Option<String> {
        use crate::setup::strings::t;
        let key = if let Some(facts) = &self.storage.facts {
            if !facts.offers_full_os() {
                return Some(
                    facts
                        .blockers
                        .iter()
                        .find(|blocker| !blocker.has_remedy())
                        .map_or_else(
                            || t("installer-review-storage-pending"),
                            |blocker| {
                                crate::setup::internal_install_flow::blocker_text(
                                    *blocker,
                                    facts.basis.as_ref().map_or(
                                        crate::setup::model::HostOs::Other,
                                        |basis| match basis.host_os() {
                                            staging::plan::HostOs::Linux => {
                                                crate::setup::model::HostOs::Linux
                                            }
                                            staging::plan::HostOs::Windows => {
                                                crate::setup::model::HostOs::Windows
                                            }
                                        },
                                    ),
                                )
                            },
                        ),
                );
            }
            if let Err(error) = self.storage.admit() {
                if let staging::plan::PlanError::InsufficientSpace {
                    needed_bytes,
                    available_bytes,
                } = error
                {
                    return Some(crate::setup::strings::t_args(
                        "installer-review-storage-shortfall",
                        &[
                            ("needed", ui::format::memory(needed_bytes).into()),
                            ("available", ui::format::memory(available_bytes).into()),
                        ],
                    ));
                }
                "installer-review-storage-pending"
            } else if self.recovery.evidence().is_err() {
                "installer-recovery-copy-required"
            } else {
                return None;
            }
        } else {
            "installer-review-storage-pending"
        };
        Some(t(key))
    }

    pub(super) fn review_storage_summary(&self) -> String {
        use crate::setup::strings::{t, t_args};
        // Recompute for display from the current facts and selected size, not
        // the cached approved geometry. This cannot authorize installation.
        let Some(basis) = self
            .storage
            .facts
            .as_ref()
            .and_then(|facts| facts.basis.as_ref())
        else {
            return t("installer-review-storage-pending");
        };
        let Ok(plan) = staging::plan::compute(
            basis,
            super::storage::requirements(self.storage.selected() << 30),
        ) else {
            return t("installer-review-storage-pending");
        };
        let allocation = t_args(
            "installer-review-allocation",
            &[("size", self.storage.selected().into())],
        );
        let effect = if let Some(shrink) = &plan.shrink {
            t_args(
                "installer-review-resize",
                &[
                    ("before", ui::format::memory(shrink.from_bytes).into()),
                    ("after", ui::format::memory(shrink.to_bytes).into()),
                ],
            )
        } else {
            t("installer-review-unallocated")
        };
        format!("{allocation} {effect}")
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
        let capture_files = self.selected_files()?;
        let wants_migration = self.journey.migrate == Some(true)
            && (capture_files || self.selected_applications() || self.selected_source_anchor());
        let migration = if wants_migration {
            Some(
                self.migration_selection
                    .reviewed_intent()
                    .ok_or("installer-preview-import-unavailable")?,
            )
        } else {
            None
        };
        if self.journey.migrate == Some(true) && capture_files && self.selected_applications() {
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
                Page::Devices
                    | Page::Purpose
                    | Page::Belongings
                    | Page::Accounts
                    | Page::Review
                    | Page::Storage
            )
            || self.journey.destination != Some(Destination::Computer)
            || self.media_preparation.busy()
            || self.personal_busy()
            || self.migration_busy()
            || self.direct.active
        {
            return Err("installer-preview-wrong-page");
        }
        if self.sensitive_handoff_deferred() {
            self.direct.belongings_pending = false;
            self.direct.belongings_continue = false;
            return Ok(());
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
                mail: self.selected_mail()?,

                other_device: None,
                tpm_target: None,
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
        if self.sensitive_handoff_deferred() {
            let tpm_target = self.tpm_target()?;
            // The package keeps identity, accounts and browser. Mail, Home and
            // applications stay in the source journal and are streamed after
            // first boot, so a mailbox larger than the 32 MiB package bound
            // never has to become installation ciphertext.
            let migration = self.deferred_migration(tpm_target.is_some())?;
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
                migration: migration.clone(),
                deferred: true,
            };
            if let Some(prepared) = self.direct.prepared_belongings.clone() {
                self.direct.pending = Some(snapshot);
                return self.finish_direct(Some(prepared));
            }
            if let Some(target) = tpm_target {
                // The package carries the approved identity and selected
                // tokens under a machine-generated secret. Mail and retained
                // files stay in the source journal, so a large mailbox never
                // becomes installation ciphertext or exceeds the package bound.
                let identity = self
                    .personal
                    .prepared_handle()
                    .ok_or("installer-preview-identity-not-prepared")?;
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
                let secret = imager_backend::setup::prepared_tpm::generate_secret()
                    .map_err(|_| "installer-preview-tpm-handoff-unavailable")?;
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
                        planned_direct: None,
                        applications: None,
                        browser: (self.journey.migrate == Some(true)
                            && self.belongings.browser.available())
                        .then(|| self.belongings.browser.selected())
                        .filter(|selected| !selected.profiles().is_empty()),
                        mail: None,
                        other_device: None,
                        tpm_target: Some(target),
                    },
                    secret,
                    Some(public),
                )?;
                self.direct.pending = Some(snapshot);
                return Ok(());
            }
            self.direct.pending = Some(snapshot);
            return self.finish_direct(None);
        }
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
            deferred: false,
        };
        if snapshot.plan.shrink.is_none()
            && let Some(prepared) = self.direct.prepared_belongings.clone()
            && prepared.matches_reviewed_belongings(&identity, &public, snapshot.migration.as_ref())
        {
            self.direct.pending = Some(snapshot);
            return self.finish_direct(Some(prepared));
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
                mail: self.selected_mail()?,

                other_device: None,
                tpm_target: None,
            },
            zeroize::Zeroizing::new(self.journey.password.clone()),
            Some(public),
        )?;
        self.direct.pending = Some(snapshot);
        Ok(())
    }

    /// Migration consent for retained original files that survives installation
    /// without carrying identity, account or file ciphertext. The selected
    /// mail, browser and Home decisions stay in a private journal inside the
    /// source Home; the plan carries only its opaque operation and digest.
    ///
    /// Writing that journal needs the retained source's ownership and mounts,
    /// which only the Linux-hosted installed system has; the backend owns that
    /// decision and refuses the preparation elsewhere.
    fn deferred_migration(
        &self,
        carried_by_package: bool,
    ) -> Result<Option<MigrationIntent>, &'static str> {
        if self.journey.migrate != Some(true) {
            return Ok(None);
        }
        if !self.selected_files()?
            && !self.selected_applications()
            && !self.selected_source_anchor()
        {
            // A complete source with nothing selected has no Home or account
            // journal to anchor. It must not capture an empty Home merely
            // because the category defaulted on.
            return Ok(None);
        }
        if !self.capabilities.prepared_installation {
            return Err("installer-preview-deferred-handoff-unavailable");
        }
        let intent = self
            .migration_selection
            .reviewed_intent()
            .ok_or("installer-preview-import-unavailable")?;
        let source_home = self
            .belongings
            .home
            .as_deref()
            .ok_or("installer-preview-home-unavailable")?;
        let exclusions = if self.selected_files()? {
            self.migration_exclusions()?
        } else {
            Vec::new()
        };
        let exclusions = exclusions
            .iter()
            .map(|path| imager_backend::setup::migration_store::NativePath::encode(path))
            .collect::<Vec<_>>();
        let mail = self
            .selected_mail()?
            .map(|selection| selection.profiles)
            .unwrap_or_default();
        // Browser profiles and selected online accounts are already inside
        // the TPM-protected package. Keeping them out of the journal prevents
        // a second bookmarks/history import or a pending GOA row replacing the
        // authentication the package carried.
        let browser = if carried_by_package {
            super::browser::SelectedSources::default()
        } else {
            self.belongings.browser.selected()
        };
        let account_sources = if carried_by_package || !self.journey.categories[2] {
            Vec::new()
        } else {
            self.accounts
                .selected_source_accounts(&self.belongings.accounts)
        };
        let plan = intent.source_plan();
        let prepared = imager_backend::setup::deferred_handoff::prepare(
            &imager_backend::setup::deferred_handoff::Prepare {
                source_home,
                plan: &plan,
                exclusions: &exclusions,
                mail: &mail,
                browser: &browser,
                applications: self.selected_applications(),
                accounts: plan.categories.accounts,
                account_sources: &account_sources,
                accounts_in_package: carried_by_package && plan.categories.accounts,
                browser_in_package: carried_by_package,
            },
        )
        .map_err(|_| "installer-preview-deferred-handoff-unavailable")?;
        intent
            .attach_deferred_handoff(prepared.binding)
            .map(Some)
            .map_err(|_| "installer-preview-deferred-handoff-unavailable")
    }

    fn tpm_target(
        &self,
    ) -> Result<Option<imager_backend::setup::prepared_tpm::TargetAcquisition>, &'static str> {
        // The live medium carries its own signed target bundle. A preparation
        // host is not that image, so it uses the small target policy bundle
        // published by the release catalog. The backend reviews either against
        // the release authority pinned into this binary, never a
        // caller-selected key.
        let policy = crate::host::architecture()
            .ok()
            .flatten()
            .and_then(|architecture| {
                crate::release::bundled_target_policy_for_arch(architecture)
                    .ok()
                    .flatten()
            });
        imager_backend::setup::prepared_tpm::target_for_preparation(
            &imager_backend::setup::prepared_tpm::AcquireRequest {
                policy: policy.as_ref(),
                cancel: &std::sync::atomic::AtomicBool::new(false),
            },
        )
        .map_err(|_| "installer-preview-tpm-handoff-unavailable")
    }

    pub(super) fn finish_direct(
        &mut self,
        prepared: Option<Arc<super::prepared_media::PreparedMedia>>,
    ) -> Result<(), &'static str> {
        let mut snapshot = self
            .direct
            .pending
            .take()
            .ok_or("installer-preview-storage-unavailable")?;
        if let Some(prepared) = &prepared {
            prepared
                .bind_recovery(
                    self.recovery
                        .evidence()
                        .map_err(|_| "installer-recovery-copy-required")?,
                    &snapshot.plan,
                )
                .map_err(|_| "installer-recovery-write-guard")?;
            snapshot.profile.prepared_setup = Some(prepared.expectation().clone());
        } else {
            // Deferred setups still require the independent recovery copy
            // review before the privileged write can begin.
            self.recovery
                .evidence()
                .map_err(|_| "installer-recovery-copy-required")?;
        }
        let child =
            crate::setup::view::SetupApp::from_guided_review(crate::setup::view::ReviewedFullOs {
                facts: snapshot.facts,
                plan: snapshot.plan,
                profile: snapshot.profile,
                migration: snapshot.migration,
                prepared,
                deferred: snapshot.deferred,
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
        self.composition_replaced = true;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_allocation_tracks_selected_space_and_method_without_granting_approval() {
        use crate::setup::strings::{t, t_args};
        use ui::theme::Theme;
        let mut preview = InstallerPreview::default();
        preview.journey.page = Page::Review;
        preview.journey.destination = Some(Destination::Computer);
        assert_eq!(
            preview.direct_storage_blocker(),
            Some(t("installer-review-storage-pending"))
        );
        assert_eq!(
            preview.review_storage_summary(),
            t("installer-review-storage-pending")
        );
        preview.storage.stage();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        let destination = root.join("recovery");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        let belongings = super::super::belongings::Belongings::from_windows_roots(&[
            crate::setup::windows_inventory::SelectedRoot {
                path: source,
                role: crate::setup::windows_inventory::RootRole::KnownFolder,
            },
        ])
        .unwrap();
        let inventory = belongings.windows.as_ref().unwrap();
        preview
            .recovery
            .review_directory(inventory, &inventory.roots[0].source_id, &destination)
            .unwrap();
        assert_eq!(
            preview.direct_storage_blocker(),
            Some(t("installer-recovery-copy-required"))
        );
        for size in [80, 120] {
            preview.storage.set_size(size).unwrap();
            let body = preview.body(&Theme::default());
            let row =
                super::super::visual_tests::control(&body, "installer.review.storage").unwrap();
            let node = row.accessibility().unwrap();
            let summary = preview.review_storage_summary();
            assert!(summary.contains(&t_args(
                "installer-review-allocation",
                &[("size", size.into())]
            )));
            assert!(summary.contains(&t("installer-review-unallocated")));
            assert!(format!("{node:?}").contains(&size.to_string()));
        }
        let basis = crate::setup::staging::plan::tests::windows_basis(512, 500, 100, false);
        preview.storage.accept(staging::probe::HostFacts {
            basis: Some(basis),
            bitlocker_protected: false,
            blockers: Vec::new(),
        });
        preview.storage.set_size(120).unwrap();
        let shrink = preview
            .storage
            .plan
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .shrink
            .as_ref()
            .unwrap();
        assert!(preview.review_storage_summary().contains(&t_args(
            "installer-review-resize",
            &[
                ("before", ui::format::memory(shrink.from_bytes).into()),
                ("after", ui::format::memory(shrink.to_bytes).into()),
            ]
        )));
        preview
            .storage
            .plan
            .as_mut()
            .unwrap()
            .as_mut()
            .unwrap()
            .target_extent
            .first_lba += 2048;
        assert!(preview.storage.admit().is_err());
        assert_eq!(
            preview.direct_storage_blocker(),
            Some(t("installer-review-storage-pending"))
        );
        assert!(!preview.direct_available());
        assert_eq!(
            preview.prepare_direct(),
            Err("installer-preview-storage-unavailable")
        );
        assert!(!preview.direct.active);
        preview.journey.edit("storage").unwrap();
        assert_eq!(preview.journey.page, Page::Storage);
        assert!(preview.journey.saving_edit());
    }

    #[test]
    fn live_review_blocker_matches_disabled_action_and_belongings_edit_says_save() {
        use crate::setup::strings::t;
        use ui::theme::Theme;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_IMAGER_ALLOW_INTERNAL_INSTALL", "1");
        let mut preview = InstallerPreview {
            live: true,
            ..Default::default()
        };
        preview.journey.page = Page::Review;
        preview.journey.destination = Some(Destination::Computer);
        let root = preview.composition(&Theme::default());
        let control = super::super::visual_tests::control;
        assert!(
            control(root.as_ref(), "installer.prepare-direct")
                .unwrap()
                .accessibility()
                .unwrap()
                .state
                .disabled
        );
        assert_eq!(
            preview.direct_blocker(),
            Some(t("installer-review-context-pending"))
        );
        let blocker = control(root.as_ref(), "installer.review.blocker")
            .unwrap()
            .accessibility()
            .unwrap();
        assert_eq!(
            blocker.name.as_deref(),
            Some(t("installer-review-context-pending").as_str())
        );
        assert!(control(root.as_ref(), "installer.review.storage").is_some());
        preview.journey.migrate = Some(true);
        preview.journey.edit("belongings-details").unwrap();
        assert!(preview.journey.saving_edit());
        let root = preview.composition(&Theme::default());
        let next = control(root.as_ref(), "installer.next")
            .unwrap()
            .accessibility()
            .unwrap();
        assert_eq!(
            next.name.as_deref(),
            Some(t("installer-preview-save").as_str())
        );
    }

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
