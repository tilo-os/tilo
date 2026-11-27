//! The direct route: Full Tilo installed beside the system the person is using.
use super::native::NativeSetup;
use super::{Destination, InstallerPreview, Page};

impl InstallerPreview {
    /// The reviewed, secret-free installation preferences: the person's
    /// choices on top of the imager's own profile, without any password.
    pub(super) fn install_profile(&self) -> Result<crate::model::InstallProfile, &'static str> {
        let mut profile = self.install_preferences.clone();
        profile.device_label.clone_from(&self.journey.device_name);
        profile.purpose.clone_from(&self.journey.purpose);
        profile.user.password.clear();
        profile.encryption_passphrase.clear();
        profile
            .validate_media_profile()
            .map_err(|_| "installer-preview-install-profile-invalid")?;
        Ok(profile)
    }

    pub(super) fn direct_available(&self) -> bool {
        self.direct_blocker().is_none()
    }

    pub(super) fn direct_blocker(&self) -> Option<String> {
        use crate::setup::strings::t;
        let key = if !self.live
            || self.journey.page != Page::Review
            || self.journey.destination != Some(Destination::Computer)
        {
            "installer-review-not-ready"
        } else if !crate::setup::internal_install_flow::live_install_allowed() {
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
                                            crate::backend::plan::HostOs::Linux => {
                                                crate::setup::model::HostOs::Linux
                                            }
                                            crate::backend::plan::HostOs::Windows => {
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
                if let crate::backend::plan::PlanError::InsufficientSpace {
                    needed_bytes,
                    available_bytes,
                } = error
                {
                    return Some(crate::setup::strings::t_args(
                        "installer-review-storage-shortfall",
                        &[
                            ("needed", tilo_ui_base::format::memory(needed_bytes).into()),
                            (
                                "available",
                                tilo_ui_base::format::memory(available_bytes).into(),
                            ),
                        ],
                    ));
                }
                "installer-review-storage-pending"
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
        let Some(basis) = self
            .storage
            .facts
            .as_ref()
            .and_then(|facts| facts.basis.as_ref())
        else {
            return t("installer-review-storage-pending");
        };
        let Ok(plan) = crate::backend::plan::compute(
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
                    (
                        "before",
                        tilo_ui_base::format::memory(shrink.from_bytes).into(),
                    ),
                    (
                        "after",
                        tilo_ui_base::format::memory(shrink.to_bytes).into(),
                    ),
                ],
            )
        } else {
            t("installer-review-unallocated")
        };
        format!("{allocation} {effect}")
    }

    /// Hand the reviewed installation to the native Full Tilo setup. The
    /// person's password and workspace are set when the installed system first
    /// starts.
    pub(super) fn prepare_direct(&mut self) -> Result<(), &'static str> {
        if !self.direct_available() {
            return Err("installer-preview-storage-unavailable");
        }
        let profile = self.install_profile()?;
        let facts = self
            .storage
            .facts
            .clone()
            .ok_or("installer-preview-storage-unavailable")?;
        let plan = self
            .storage
            .plan
            .as_ref()
            .and_then(|plan| plan.as_ref().ok())
            .cloned()
            .ok_or("installer-preview-storage-unavailable")?;
        let native =
            NativeSetup::from_guided_review(crate::setup::internal_install_flow::ReviewedFullOs {
                facts,
                plan,
                profile,
                deferred: true,
            })
            .map_err(|_| "installer-preview-direct-review-unavailable")?;
        self.open_native(native, false);
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    tilo_test_env::scenario! {
        fn the_direct_installation_follows_the_review_and_the_chosen_name;

        phase review_allocation_tracks_selected_space_and_method_without_granting_approval;
        {
            use crate::setup::strings::{t, t_args};
            use tilo_ui_base::theme::Theme;
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
            let basis = crate::backend::plan::tests::windows_basis(512, 500, 100, false);
            preview.storage.accept(crate::setup::staging::probe::HostFacts {
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
                    ("before", tilo_ui_base::format::memory(shrink.from_bytes).into()),
                    ("after", tilo_ui_base::format::memory(shrink.to_bytes).into()),
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
            assert!(preview.native.is_none());
            preview.journey.edit("storage").unwrap();
            assert_eq!(preview.journey.page, Page::Storage);
            assert!(preview.journey.saving_edit());
        }

        phase live_review_blocker_matches_disabled_action_and_devices_edit_says_save;
        {
            use crate::setup::strings::t;
            use tilo_ui_base::theme::Theme;
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
                Some(t("installer-review-storage-pending"))
            );
            let blocker = control(root.as_ref(), "installer.review.blocker")
                .unwrap()
                .accessibility()
                .unwrap();
            assert_eq!(
                blocker.name.as_deref(),
                Some(t("installer-review-storage-pending").as_str())
            );
            assert!(control(root.as_ref(), "installer.review.storage").is_some());
            preview.journey.edit("devices").unwrap();
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

        phase direct_refuses_missing_review_without_discarding_choices;
        {
            let mut preview = InstallerPreview {
                live: true,
                ..Default::default()
            };
            preview.journey.page = Page::Storage;
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.device_name = "Chosen name".into();
            preview.journey.password = "private chosen password".into();
            assert_eq!(
                preview.prepare_direct(),
                Err("installer-preview-storage-unavailable")
            );
            assert_eq!(preview.journey.device_name, "Chosen name");
            assert_eq!(preview.journey.password, "private chosen password");
            assert!(preview.native.is_none());
        }

        phase chosen_device_name_reaches_the_media_profile;
        {
            let mut preview = InstallerPreview::default();
            preview.journey.device_name = "Ada’s workstation 🌿".into();
            let draft = preview.install_profile().unwrap();
            assert_eq!(draft.device_label, preview.journey.device_name);
            let media = crate::setup::staging::handoff::profile_from_install(&draft).unwrap();
            assert_eq!(media["device_label"], preview.journey.device_name);
            preview.journey.device_name = "é".repeat(32);
            assert_eq!(
                preview.install_profile().unwrap().device_label,
                preview.journey.device_name
            );
            preview.journey.device_name = "é".repeat(33);
            assert!(preview.install_profile().is_err());
            assert_eq!(preview.journey.device_name.len(), 66);
            preview.journey.device_name = "invalid\nname".into();
            assert!(preview.install_profile().is_err());
        }

        phase install_preferences_carry_no_passwords;
        {
            let mut preview = InstallerPreview::default();
            preview.install_preferences.user.password = "must not travel".into();
            preview.install_preferences.encryption_passphrase = "also private".into();
            let profile = preview.install_profile().unwrap();
            assert!(profile.user.password.is_empty());
            assert!(profile.encryption_passphrase.is_empty());
            preview.install_preferences.user.username = "root".into();
            assert!(preview.install_profile().is_err());
        }
    }
}
