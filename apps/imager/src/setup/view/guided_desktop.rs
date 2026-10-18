//! Guided Desktop package setup retains prepared custody until native delivery completes.
use super::*;
use crate::setup::journey::prepared_media::PreparedMedia;

pub(super) struct DesktopPreparation {
    pub(super) prepared: Arc<PreparedMedia>,
    home: PathBuf,
    pub(super) pending: Option<Receiver<Result<(), String>>>,
    delivered: bool,
    pub(super) failed: bool,
}
impl SetupApp {
    pub(crate) fn from_guided_desktop(
        prepared: Arc<PreparedMedia>,
        home: PathBuf,
    ) -> Result<Self, String> {
        let mut app = Self::new();
        app.controller.borrow_mut().review_desktop();
        app.desktop_only = true;
        app.chosen_experience = Some(Experience::Desktop);
        app.guided_mode = true;
        app.desktop_flow
            .require_account_protocol(prepared.account_custody_protocol());
        app.desktop_flow
            .require_service_session_protocol(prepared.service_session_protocol());
        app.guided_desktop = Some(DesktopPreparation {
            prepared,
            home,
            pending: None,
            delivered: false,
            failed: false,
        });
        app.refresh_guided_navigation();
        app.status.clear();
        app.status_is_error = false;
        app.composed.invalidate();
        Ok(app)
    }
    pub(super) fn poll_guided_desktop(&mut self) -> bool {
        let Some(preparation) = &mut self.guided_desktop else {
            return false;
        };
        if let Some(receiver) = &preparation.pending {
            let result = match receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return false,
                Err(mpsc::TryRecvError::Disconnected) => Err(super::super::strings::t(
                    "installer-desktop-delivery-failed",
                )),
            };
            preparation.pending = None;
            if result == Ok(()) {
                preparation.delivered = true;
                self.status = super::super::strings::t("installer-desktop-ready");
                self.status_is_error = false;
            } else {
                preparation.failed = true;
                self.status = super::super::strings::t("installer-desktop-delivery-failed");
                self.status_is_error = true;
            }
            self.composed.invalidate();
            return true;
        }
        if preparation.delivered
            || preparation.failed
            || !matches!(
                self.desktop_flow.state(),
                DesktopFlowState::Installed { .. }
            )
        {
            return false;
        }
        let expected_consumer = match self.desktop_flow.selection().and_then(|selection| {
            (preparation.prepared.account_custody_protocol().is_some())
                .then(|| selection.release.account_custody.clone())
        }) {
            Some(Some(proof)) => Some(proof),
            Some(None) => {
                preparation.failed = true;
                self.status = super::super::strings::t("imager-setup-account-consumer-unavailable");
                self.status_is_error = true;
                self.composed.invalidate();
                return true;
            }
            None => None,
        };
        let prepared = preparation.prepared.clone();
        let home = preparation.home.clone();
        let (sender, receiver) = mpsc::channel();
        preparation.pending = Some(receiver);
        self.status = super::super::strings::t("installer-desktop-delivering");
        self.status_is_error = false;
        std::thread::spawn(move || {
            let admission = expected_consumer.map_or(Ok(()), |expected| {
                super::super::account_admission::installed().and_then(|actual| {
                    if actual == expected {
                        Ok(())
                    } else {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "Installed consumers differ from selected package",
                        ))
                    }
                })
            });
            let result = admission
                .and_then(|()| {
                    super::super::desktop_delivery::deliver(
                        prepared,
                        &home,
                        &AtomicBool::new(false),
                    )
                    .map(|_| ())
                })
                .map_err(|_| super::super::strings::t("installer-desktop-delivery-failed"));
            let _ = sender.send(result);
        });
        self.composed.invalidate();
        true
    }
    pub(super) fn guided_desktop_can_navigate(&self) -> bool {
        self.guided_desktop
            .as_ref()
            .is_none_or(|preparation| preparation.pending.is_none() && !preparation.delivered)
            && !self.desktop_flow.has_pending_operation()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn desktop_only_records_intent_without_inventing_support_or_preparation() {
        let app = SetupApp::for_desktop();
        assert_eq!(app.controller().page(), SetupPage::Review);
        assert_eq!(
            app.controller().chosen_experience(),
            Some(Experience::Desktop)
        );
        assert!(!app.capabilities.platform.desktop_session);
        assert!(!app.known_folder_lookup_pending);
        assert!(!app.internal_install.is_probing());
        assert!(app.guided_desktop.is_none());
        assert!(!app.setup_projection().desktop.actions.install);
    }

    #[test]
    fn desktop_only_install_checks_packages_and_blocks_navigation_without_identity() {
        let mut app = SetupApp::for_desktop();
        app.desktop_flow = DesktopFlow::with_runtime_and_state(
            crate::setup::desktop_flow::DesktopRuntime {
                launch_probe: || {
                    let (_sender, receiver) = mpsc::channel();
                    Ok(receiver)
                },
                launch_download: |_, _| Err("unexpected download".into()),
                launch_install: |_| Err("unexpected native install".into()),
            },
            DesktopFlowState::Ready(fixture_desktop_selection(false)),
        );
        assert!(!app.capabilities.platform.desktop_session);
        assert!(app.setup_projection().desktop.actions.install);
        app.answer(SetupEffect::InstallDesktop).unwrap();
        assert!(app.desktop_flow.has_pending_operation());
        assert!(matches!(
            app.desktop_flow.state(),
            DesktopFlowState::Checking
        ));
        assert!(!app.guided_navigation.get());
        assert!(app.answer(SetupEffect::GoBackInSetup).is_err());
        assert!(app.answer(SetupEffect::InstallDesktop).is_err());
        assert!(app.guided_desktop.is_none());
        assert!(app.controller().folders().is_empty());
        assert!(!app.known_folder_lookup_pending);
        assert!(!app.internal_install.is_probing());
    }

    fn prepared(root: &Path) -> Arc<PreparedMedia> {
        let identity = crate::setup::prepared_identity::PreparedIdentity::create(
            &root.join("identity"),
            "Chosen person",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::for_session(),
        )
        .unwrap();
        PreparedMedia::prepare(
            &identity,
            None,
            zeroize::Zeroizing::new("chosen preparation password".into()),
            "desktop-session".into(),
        )
        .unwrap()
    }
    fn root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
    #[test]
    fn guided_desktop_delivers_actual_preparation_after_verified_package_and_keeps_current_files() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let root = root();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(home.join("ordinary-document"), b"current document").unwrap();
        let prepared = prepared(root.path());
        let artifact = prepared.staging_artifact().unwrap();
        let mut app = SetupApp::from_guided_desktop(prepared, home.clone()).unwrap();
        app.desktop_flow = DesktopFlow::from_fixture_state(DesktopFlowState::Installed {
            selection: fixture_desktop_selection(true),
            result: crate::setup::desktop_install::InstalledDesktop {
                version: "fixture".into(),
                session_entry: "/usr/share/wayland-sessions/tilo.desktop".into(),
            },
        });
        assert!(!home.join(".local/state/tilo/prepared-setup").exists());
        assert!(app.poll_guided_desktop());
        app.refresh_guided_navigation();
        assert!(!app.guided_navigation.get());
        let deadline = tilo_test_env::hang_deadline();
        while !app.guided_desktop.as_ref().unwrap().delivered {
            app.poll_guided_desktop();
            assert!(!app.status_is_error, "{}", app.status);
            assert!(
                std::time::Instant::now() < deadline,
                "desktop delivery did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let target = home.join(".local/state/tilo/prepared-setup");
        assert_eq!(
            std::fs::read(target.join("prepared-setup.sealed")).unwrap(),
            std::fs::read(artifact.source_path).unwrap()
        );
        assert_eq!(
            serde_json::from_slice::<crate::model::PreparedSetupExpectation>(
                &std::fs::read(target.join("receipt.json")).unwrap()
            )
            .unwrap(),
            artifact.expectation
        );
        assert_eq!(
            std::fs::read(home.join("ordinary-document")).unwrap(),
            b"current document"
        );
        assert!(app.controller().folders().is_empty());
        assert!(!app.status_is_error);
        assert_eq!(
            app.status,
            crate::setup::strings::t("installer-desktop-ready")
        );
    }
    #[test]
    fn guided_desktop_semantic_install_rechecks_canonical_package_flow_and_blocks_back() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let root = root();
        let mut app =
            SetupApp::from_guided_desktop(prepared(root.path()), root.path().join("home")).unwrap();
        app.desktop_flow = DesktopFlow::with_runtime_and_state(
            crate::setup::desktop_flow::DesktopRuntime {
                launch_probe: || {
                    let (_sender, receiver) = mpsc::channel();
                    Ok(receiver)
                },
                launch_download: |_, _| Err("unexpected download".into()),
                launch_install: |_| Err("unexpected native install".into()),
            },
            DesktopFlowState::Ready(fixture_desktop_selection(false)),
        );
        assert!(app.live_rows().iter().any(|row| {
            row.clone()
                .into_resource()
                .children
                .iter()
                .any(|child| child.id.as_str() == "imager.setup.desktop")
        }));
        app.answer(SetupEffect::InstallDesktop).unwrap();
        assert!(app.desktop_flow.has_pending_operation());
        assert!(matches!(
            app.desktop_flow.state(),
            DesktopFlowState::Checking
        ));
        assert!(!app.guided_navigation.get());
        assert!(app.answer(SetupEffect::InstallDesktop).is_err());
        assert!(app.answer(SetupEffect::GoBackInSetup).is_err());
        assert!(!root.path().join("home/.local").exists());
    }
}
