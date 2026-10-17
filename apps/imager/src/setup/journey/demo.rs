//! Interactive use of the existing screenshot fixture, never native controllers.
//!
//! Keep admission fail-closed: a newly added effect is not safe merely because
//! its button is hidden. Both queued controls and semantic invocations use this
//! list before entering the production action handler.

use super::InstallerPreviewEffect;
use ui::tilo_resource::Effect as _;

pub(super) fn admits(effect: &InstallerPreviewEffect) -> bool {
    admits_id(effect.id())
}

pub(super) fn admits_id(id: &str) -> bool {
    matches!(
        id,
        "read-preview"
            | "choose-preview"
            | "set-preview-name"
            | "set-preview-device-name"
            | "set-preview-password"
            | "set-preview-confirmation"
            | "set-preview-category"
            | "toggle-preview-details"
            | "toggle-flatpak-access-details"
            | "toggle-storage-details"
            | "set-storage-size"
            | "continue-preview"
            | "back-in-preview"
            | "restart-preview"
            | "edit-preview"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::GuidedInstallerShell;
    use ui::framework::testing::TestApp;

    fn state(app: &mut TestApp<GuidedInstallerShell>) -> serde_json::Value {
        app.effect(&InstallerPreviewEffect::ReadPreview)
            .unwrap()
            .value
            .unwrap()
    }

    fn assert_demo_notice(app: &mut TestApp<GuidedInstallerShell>) {
        app.settle();
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            let notice = super::super::visual_tests::control(preview, "installer.demo-notice")
                .expect("every demo page must identify its example data");
            assert_eq!(
                notice.accessibility().unwrap().name.as_deref(),
                Some("Preview only · Example data. Nothing will be installed or connected.")
            );
            let rect = notice.get_rect();
            assert!(rect.width() > 0.0 && rect.height() > 0.0, "{rect:?}");
            assert!(rect.y0 >= 0.0 && rect.y1 <= 820.0, "{rect:?}");
        })
        .unwrap();
    }

    fn advance(app: &mut TestApp<GuidedInstallerShell>) {
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_demo_notice(app);
    }

    #[test]
    fn ordinary_capture_has_no_demo_notice() {
        let mut app =
            TestApp::<GuidedInstallerShell>::capture_at(Some("installer-destination"), (1280, 720))
                .unwrap();
        app.settle();
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            assert!(!preview.demo);
            assert!(
                super::super::visual_tests::control(preview, "installer.demo-notice").is_none()
            );
        })
        .unwrap();
    }

    #[test]
    fn launched_demo_finishes_through_production_actions_without_native_controllers() {
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--demo"]).unwrap();
        assert_eq!(state(&mut app)["page"], "destination");
        assert_demo_notice(&mut app);
        for choice in ["computer", "migrate", "new"] {
            app.effect(&InstallerPreviewEffect::ChoosePreview {
                choice: choice.into(),
            })
            .unwrap();
            advance(&mut app);
        }
        assert_eq!(state(&mut app)["page"], "profile");
        advance(&mut app);
        for effect in [
            InstallerPreviewEffect::SetPreviewPassword {
                value: "demo password only".into(),
            },
            InstallerPreviewEffect::SetPreviewConfirmation {
                value: "demo password only".into(),
            },
        ] {
            app.effect(&effect).unwrap();
        }
        advance(&mut app);
        assert_eq!(state(&mut app)["page"], "devices");
        advance(&mut app);
        assert_eq!(state(&mut app)["page"], "purpose");
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "computer".into(),
        })
        .unwrap();
        advance(&mut app);
        for page in ["belongings", "accounts", "storage", "review"] {
            assert_eq!(state(&mut app)["page"], page);
            if page == "review" {
                for effect in [
                    InstallerPreviewEffect::PrepareUsb,
                    InstallerPreviewEffect::PrepareDirectInstallation,
                    InstallerPreviewEffect::PrepareDesktopInstallation,
                ] {
                    assert!(app.effect(&effect).is_err());
                }
                assert!(
                    app.effect(&crate::resource::ImagerEffect::Write {
                        device: "/dev/sda".into(),
                        confirmation: "sda".into(),
                    })
                    .is_err()
                );
            }
            advance(&mut app);
        }
        assert_eq!(state(&mut app)["page"], "ready");
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            assert!(preview.demo);
            assert!(!preview.live);
            assert!(preview.capture);
            assert!(preview.dispatcher.is_none());
            assert!(preview.direct.context.is_none());
            assert!(preview.belongings_worker.is_none());
            assert!(preview.personal.prepared_handle().is_none());
            assert!(!preview.direct.active);
            assert!(preview.media.as_ref().is_none_or(|media| !media.get()));
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::RestartPreview).unwrap();
        assert_eq!(state(&mut app)["page"], "destination");
        assert_demo_notice(&mut app);
        app.with_widget::<super::super::InstallerPreview, _>(|preview| {
            assert!(preview.demo);
            assert!(!preview.live);
        })
        .unwrap();
    }

    #[test]
    fn every_destination_and_identity_route_remains_navigable() {
        for destination in ["computer", "another", "desktop"] {
            for identity in ["new", "existing"] {
                let mut app = TestApp::<GuidedInstallerShell>::launched(&["--demo"]).unwrap();
                if destination == "desktop" {
                    app.effect(&InstallerPreviewEffect::ChoosePreview {
                        choice: destination.into(),
                    })
                    .unwrap();
                    advance(&mut app);
                    assert_eq!(state(&mut app)["page"], "review");
                    app.with_widget::<super::super::InstallerPreview, _>(|preview| {
                        assert!(preview.journey.identity.is_none());
                        assert!(preview.journey.name.is_empty());
                        assert!(preview.journey.password.is_empty());
                        assert_eq!(preview.journey.categories, [false; 3]);
                        assert!(preview.personal.prepared_handle().is_none());
                    })
                    .unwrap();
                    app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
                    assert_eq!(state(&mut app)["page"], "destination");
                    continue;
                }
                for choice in [destination, "migrate", identity] {
                    app.effect(&InstallerPreviewEffect::ChoosePreview {
                        choice: choice.into(),
                    })
                    .unwrap();
                    advance(&mut app);
                }
                if identity == "existing" {
                    app.effect(&InstallerPreviewEffect::ChoosePreview {
                        choice: "backup".into(),
                    })
                    .unwrap();
                }
                advance(&mut app);
                app.effect(&InstallerPreviewEffect::SetPreviewPassword {
                    value: "demo password only".into(),
                })
                .unwrap();
                app.effect(&InstallerPreviewEffect::SetPreviewConfirmation {
                    value: "demo password only".into(),
                })
                .unwrap();
                advance(&mut app);
                advance(&mut app);
                if destination != "desktop" {
                    app.effect(&InstallerPreviewEffect::ChoosePreview {
                        choice: "computer".into(),
                    })
                    .unwrap();
                    advance(&mut app);
                }
                for page in ["belongings", "accounts", "storage", "review"] {
                    assert_eq!(state(&mut app)["page"], page, "{destination}/{identity}");
                    advance(&mut app);
                }
                assert_eq!(state(&mut app)["page"], "ready");
            }
        }
    }

    #[test]
    fn demo_refuses_external_effects_at_the_semantic_door() {
        let mut app = TestApp::<GuidedInstallerShell>::launched(&["--demo"]).unwrap();
        for effect in [
            InstallerPreviewEffect::PrepareUsb,
            InstallerPreviewEffect::PrepareDirectInstallation,
            InstallerPreviewEffect::PrepareDesktopInstallation,
            InstallerPreviewEffect::PrepareBelongings,
            InstallerPreviewEffect::CheckBelongings,
            InstallerPreviewEffect::CheckStorage,
            InstallerPreviewEffect::FindWindowsRoots,
            InstallerPreviewEffect::StartExistingDevicePairing,
            InstallerPreviewEffect::ChoosePreviewAvatar,
            InstallerPreviewEffect::SelectPreviewAvatar {
                source_file: "/etc/passwd".into(),
            },
            InstallerPreviewEffect::SelectMigrationHome {
                source: "/home".into(),
            },
            InstallerPreviewEffect::ExportPreparedIdentity {
                destination: "/dev/sda".into(),
            },
        ] {
            assert!(!admits(&effect));
            assert!(
                app.effect(&effect).is_err(),
                "{} must be refused",
                effect.id()
            );
        }
        assert_eq!(state(&mut app)["page"], "destination");
    }
}
