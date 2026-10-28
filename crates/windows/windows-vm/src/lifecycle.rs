//! Typed lifecycle operations exposed to the Tilo management service.
//!
//! This module intentionally has no UI and no policy prompts.  The session
//! service resolves polkit and workspace ownership before calling these
//! methods; this crate enforces that every operation targets the one Tilo
//! domain and that internet access is a short-lived, purpose-bound lease.

use thiserror::Error;

use crate::{
    ApplianceConfig, AppliancePhase, ApplianceState, DomainState, EgressLease, EgressPurpose,
    ProvisioningError, ProvisioningStore, VmBackend, VmBackendError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleAction {
    Start,
    Shutdown,
    ForceShutdown,
    Restart,
    Suspend,
    Resume,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmStatus {
    pub appliance: ApplianceState,
    pub domain: DomainState,
    /// Tilo intentionally supports one owner-managed Windows user/session.
    /// This is a state label, not a Windows username or credential.
    pub managed_session: ManagedSessionState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagedSessionState {
    Unknown,
    NotProvisioned,
    AwaitingConsoleSetup,
    Ready,
    NeedsRepair,
}

impl ManagedSessionState {
    #[must_use]
    pub const fn from_phase(phase: &AppliancePhase) -> Self {
        match phase {
            AppliancePhase::NotProvisioned => Self::NotProvisioned,
            AppliancePhase::Installing { .. } | AppliancePhase::AwaitingGuestBridge => {
                Self::AwaitingConsoleSetup
            }
            AppliancePhase::Ready => Self::Ready,
            AppliancePhase::RepairRequired { .. } => Self::NeedsRepair,
        }
    }
}

pub struct WindowsVmService<B> {
    config: ApplianceConfig,
    state: ProvisioningStore,
    backend: B,
}

impl<B: VmBackend> WindowsVmService<B> {
    #[must_use]
    pub fn new(config: ApplianceConfig, state: ProvisioningStore, backend: B) -> Self {
        Self {
            config,
            state,
            backend,
        }
    }

    pub fn status(&self) -> Result<VmStatus, LifecycleError> {
        let appliance = self.state.load()?;
        let domain = self.backend.state(&self.config)?;
        Ok(VmStatus {
            managed_session: ManagedSessionState::from_phase(&appliance.phase),
            appliance,
            domain,
        })
    }

    /// Lifecycle actions are idempotent where doing so is safe. A graceful
    /// shutdown never escalates to `destroy`; callers choose force shutdown
    /// after presenting a bounded wait/recovery affordance in the UI.
    pub fn action(&self, action: LifecycleAction) -> Result<(), LifecycleError> {
        let state = self.state.load()?;
        if matches!(action, LifecycleAction::Start | LifecycleAction::Restart)
            && matches!(state.phase, AppliancePhase::NotProvisioned)
        {
            return Err(LifecycleError::NotProvisioned);
        }
        self.backend.ensure_singleton(&self.config)?;
        match action {
            LifecycleAction::Start => self.backend.start(&self.config)?,
            LifecycleAction::Shutdown => self.backend.shutdown(&self.config)?,
            LifecycleAction::ForceShutdown => self.backend.force_shutdown(&self.config)?,
            LifecycleAction::Restart => self.backend.restart(&self.config)?,
            LifecycleAction::Suspend => self.backend.suspend(&self.config)?,
            LifecycleAction::Resume => self.backend.resume(&self.config)?,
        }
        Ok(())
    }

    /// Opens egress only for activation or Windows Update. The lease is live
    /// only; a VM restart drops the interface and callers must retry through
    /// this method. TTL is capped in the libvirt backend as a second guard.
    pub fn open_egress(
        &self,
        purpose: EgressPurpose,
        ttl_seconds: u64,
    ) -> Result<EgressLease, LifecycleError> {
        if ttl_seconds == 0 {
            return Err(LifecycleError::InvalidEgressTtl);
        }
        let state = self.state.load()?;
        if state.phase != AppliancePhase::Ready {
            return Err(LifecycleError::NotReady);
        }
        let now = tilo_clock::try_unix_now().ok_or(LifecycleError::Clock)?;
        Ok(self
            .backend
            .open_egress(&self.config, purpose, now, ttl_seconds)?)
    }

    pub fn close_egress(&self, lease: &EgressLease) -> Result<(), LifecycleError> {
        self.backend.close_egress(&self.config, lease)?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("the Windows appliance is not provisioned")]
    NotProvisioned,
    #[error("the Windows appliance is not ready for this operation")]
    NotReady,
    #[error("egress lease duration must be positive")]
    InvalidEgressTtl,
    #[error("system clock is before the Unix epoch")]
    Clock,
    #[error(transparent)]
    Provisioning(#[from] ProvisioningError),
    #[error(transparent)]
    Backend(#[from] VmBackendError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DomainState;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    struct Backend {
        state: DomainState,
        actions: Arc<Mutex<Vec<LifecycleAction>>>,
    }

    impl VmBackend for Backend {
        fn ensure_singleton(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            Ok(())
        }
        fn retire_installation_media(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            Ok(())
        }
        fn state(&self, _: &ApplianceConfig) -> Result<DomainState, VmBackendError> {
            Ok(self.state)
        }
        fn start(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            self.actions.lock().unwrap().push(LifecycleAction::Start);
            Ok(())
        }
        fn shutdown(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            self.actions.lock().unwrap().push(LifecycleAction::Shutdown);
            Ok(())
        }
        fn force_shutdown(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            self.actions
                .lock()
                .unwrap()
                .push(LifecycleAction::ForceShutdown);
            Ok(())
        }
        fn restart(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            self.actions.lock().unwrap().push(LifecycleAction::Restart);
            Ok(())
        }
        fn suspend(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            self.actions.lock().unwrap().push(LifecycleAction::Suspend);
            Ok(())
        }
        fn resume(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            self.actions.lock().unwrap().push(LifecycleAction::Resume);
            Ok(())
        }
    }

    fn service(directory: &TempDir, backend: Backend) -> WindowsVmService<Backend> {
        let store = ProvisioningStore::new(directory.path().join("state.json"));
        store
            .save(&ApplianceState {
                phase: AppliancePhase::Ready,
                license: crate::LicenseState::DigitallyActivated,
                ..ApplianceState::default()
            })
            .unwrap();
        WindowsVmService::new(
            ApplianceConfig::recommended(
                "01234567-89ab-cdef-0123-456789abcdef",
                "/var/lib/tilo/windows/system.qcow2",
                "/var/lib/tilo/windows/nvram.fd",
            ),
            store,
            backend,
        )
    }

    #[test]
    fn lifecycle_status_and_actions_are_typed_and_never_wake_or_duplicate() {
        {
            let directory = TempDir::new().unwrap();
            let service = service(
                &directory,
                Backend {
                    state: DomainState::ShutOff,
                    actions: Arc::new(Mutex::new(Vec::new())),
                },
            );
            let status = service.status().unwrap();
            assert_eq!(status.domain, DomainState::ShutOff);
            assert_eq!(status.managed_session, ManagedSessionState::Ready);
            assert_eq!(
                status.appliance.license,
                crate::LicenseState::DigitallyActivated
            );
        }
        {
            let directory = TempDir::new().unwrap();
            let backend = Backend {
                state: DomainState::Running,
                actions: Arc::new(Mutex::new(Vec::new())),
            };
            let actions = backend.actions.clone();
            let service = service(&directory, backend);
            service.action(LifecycleAction::Shutdown).unwrap();
            service.action(LifecycleAction::Resume).unwrap();
            assert_eq!(
                *actions.lock().unwrap(),
                vec![LifecycleAction::Shutdown, LifecycleAction::Resume]
            );
        }
        {
            let directory = TempDir::new().unwrap();
            let store = ProvisioningStore::new(directory.path().join("state.json"));
            let service = WindowsVmService::new(
                ApplianceConfig::recommended(
                    "01234567-89ab-cdef-0123-456789abcdef",
                    "/var/lib/tilo/windows/system.qcow2",
                    "/var/lib/tilo/windows/nvram.fd",
                ),
                store,
                Backend {
                    state: DomainState::Absent,
                    actions: Arc::new(Mutex::new(Vec::new())),
                },
            );
            assert!(matches!(
                service.action(LifecycleAction::Start),
                Err(LifecycleError::NotProvisioned)
            ));
        }
    }
}
