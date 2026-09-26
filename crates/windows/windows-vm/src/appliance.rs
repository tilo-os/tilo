use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

use crate::libvirt::VmBackendError;
use crate::remote_app::validate_share_path;
use crate::{
    AgentError, ApplianceConfig, ApplianceState, Catalog, CatalogError, DomainState, GuestAgent,
    InstallationOperationId, InstallationStatus, InstallerPackage, PreparedInstallation,
    PreparedUninstallation, ProvisioningError, ProvisioningStore, PublicationResult,
    RemoteAppError, RemoteAppLauncher, RemoteAppProcess, StagedInstaller,
    UninstallationOperationId, UninstallationStatus, VmBackend,
};

pub const HOST_PROTOCOL_MAJOR: u16 = 1;

/// An immutable projection of grants already resolved by Tilo's workspace
/// permission service. This is transport input, not a second policy database.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VmLaunchGrants {
    /// Exact canonical `Scope::NetHost` values resolved for this application.
    /// An empty list means no egress. This is deliberately not a bool: a
    /// filtered grant must retain the names it authorizes until the relay
    /// checks each outbound CONNECT request.
    pub network_hosts: Vec<String>,
    pub network: bool,
    pub clipboard: bool,
    pub microphone: bool,
    /// Opaque portal document grants.  The broker resolves these to narrowly
    /// scoped staging paths.  Keeping the access mode beside the opaque handle
    /// is important: a read-only portal decision must not become a writable
    /// RDP drive merely because the materialising broker was confused.
    pub portal_documents: Vec<PortalDocumentGrant>,
}

/// Access granted by Tilo's file portal for one opaque document handle.
///
/// This is deliberately not a filesystem permission or a host path.  The
/// capability broker is the only component allowed to turn it into a private
/// per-launch projection for `FreeRDP`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortalDocumentAccess {
    ReadOnly,
    ReadWrite,
}

impl PortalDocumentAccess {
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

/// A document the user selected through Tilo's file portal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortalDocumentGrant {
    /// An unguessable, broker-owned portal document handle.  It is never a
    /// host path and is not presented to the guest or `FreeRDP`.
    pub handle: String,
    pub access: PortalDocumentAccess,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchRequest {
    pub workspace_id: String,
    pub application_id: String,
    pub grants: VmLaunchGrants,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShareBinding {
    pub drive_name: String,
    /// Portal document this staging mount represents.
    pub portal_document_id: String,
    /// Broker-owned bind mount below the lease root. `read_only` is enforced
    /// on this mount; it is not merely a hint to `FreeRDP`.
    pub host_path: PathBuf,
    pub read_only: bool,
}

/// Short-lived resources materialised by the existing Tilo capability service.
/// Dropping or explicitly revoking `lease_id` is the broker's responsibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrokeredLaunch {
    pub lease_id: String,
    pub network: bool,
    /// Guest-local proxy configuration for the materialized network lease.
    /// It is present exactly when `network` is true and is sent only to the
    /// authenticated guest bridge, never to a manager or RDP argv.
    pub network_binding: Option<crate::GuestNetworkBinding>,
    pub clipboard: bool,
    pub microphone: bool,
    pub shares: Vec<ShareBinding>,
}

impl BrokeredLaunch {
    #[must_use]
    pub fn deny_all(lease_id: impl Into<String>) -> Self {
        Self {
            lease_id: lease_id.into(),
            network: false,
            network_binding: None,
            clipboard: false,
            microphone: false,
            shares: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), RemoteAppError> {
        if self.lease_id.is_empty()
            || !self
                .lease_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(RemoteAppError::InvalidLease);
        }
        let lease_root = crate::portal_lease_root().join(&self.lease_id);
        if self.network != self.network_binding.is_some() {
            return Err(RemoteAppError::InvalidNetworkLease);
        }
        let mut names = HashSet::new();
        let mut documents = HashSet::new();
        for share in &self.shares {
            if share.drive_name.is_empty()
                || share.drive_name.len() > 12
                || !share
                    .drive_name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric())
            {
                return Err(RemoteAppError::InvalidDriveName);
            }
            if !names.insert(share.drive_name.as_str()) {
                return Err(RemoteAppError::DuplicateDriveName);
            }
            if share.portal_document_id.trim().is_empty()
                || !documents.insert(share.portal_document_id.as_str())
            {
                return Err(RemoteAppError::InvalidPortalDocument);
            }
            validate_share_path(&share.host_path)?;
            if !share.host_path.starts_with(&lease_root) {
                return Err(RemoteAppError::ShareOutsideLease);
            }
        }
        Ok(())
    }
}

pub trait CapabilityBroker: Send + Sync {
    /// Opens egress and portal exports only for fields present in the resolved
    /// Tilo grant snapshot. Implementations must return clipboard/microphone no
    /// broader than requested; the appliance verifies that invariant.
    fn materialize(&self, request: &LaunchRequest) -> Result<BrokeredLaunch, LaunchError>;

    fn revoke(&self, lease_id: &str);
}

#[derive(Debug)]
pub struct RunningRemoteApp {
    pub application_id: String,
    pub workspace_id: String,
    pub lease_id: String,
    pub process: RemoteAppProcess,
}

pub struct WindowsAppliance<B, A, C, L> {
    config: ApplianceConfig,
    state: ProvisioningStore,
    backend: B,
    agent: A,
    capabilities: C,
    launcher: L,
    agent_timeout: Duration,
}

impl<B, A, C, L> WindowsAppliance<B, A, C, L> {
    #[must_use]
    pub fn new(
        config: ApplianceConfig,
        state: ProvisioningStore,
        backend: B,
        agent: A,
        capabilities: C,
        launcher: L,
    ) -> Self {
        Self {
            config,
            state,
            backend,
            agent,
            capabilities,
            launcher,
            agent_timeout: Duration::from_secs(45),
        }
    }

    #[must_use]
    pub fn with_agent_timeout(mut self, timeout: Duration) -> Self {
        self.agent_timeout = timeout;
        self
    }
}

impl<B, A, C, L> WindowsAppliance<B, A, C, L>
where
    B: VmBackend,
    A: GuestAgent,
    C: CapabilityBroker,
    L: RemoteAppLauncher,
{
    /// Returns the provisioned state which gates normal application launches.
    ///
    /// This deliberately does not boot the guest or attach a viewer.  The
    /// Windows appliance is a fallback resource, so a manager refresh must
    /// not wake it merely to paint an availability badge.  [`Self::launch`]
    /// performs the authenticated guest/presentation probe immediately before
    /// an explicit launch.
    pub fn launch_readiness(&self) -> Result<ApplianceState, LaunchError> {
        self.ready_state()
    }

    /// Returns the last complete catalogue accepted from the authenticated
    /// guest bridge. This deliberately performs no VM, guest-agent, or viewer
    /// operation, so manager refreshes do not wake the fallback appliance.
    ///
    /// A missing, invalid, or generation-mismatched cache is not usable. It
    /// therefore produces no application rows until a later explicit launch
    /// refreshes the authenticated publication.
    pub fn cached_catalog(&self) -> Result<Option<Catalog>, LaunchError> {
        let state = self.ready_state()?;
        let Some(catalog) = state.cached_catalog else {
            return Ok(None);
        };
        catalog.validate()?;
        if catalog.generation != state.catalog_generation {
            return Ok(None);
        }
        Ok(Some(catalog))
    }

    /// Refreshes the app catalogue only through the authenticated guest
    /// bridge. An RDP server alone is never enough to publish applications.
    pub fn catalog(&self) -> Result<Catalog, LaunchError> {
        let state = self.ensure_ready()?;
        let session = self.agent.wait_ready(self.agent_timeout)?;
        validate_health(&state, &session.health)?;
        let catalog = self.agent.catalog(&session)?;
        self.accept_catalog(state, &catalog)?;
        Ok(catalog)
    }

    /// Streams a portal-owned package through the authenticated guest bridge.
    /// The reader is not a path capability and is consumed at most once.
    pub fn stage_installer<R>(
        &self,
        package: &InstallerPackage,
        reader: R,
    ) -> Result<StagedInstaller, LaunchError>
    where
        R: Read + Send + 'static,
    {
        let (_, session) = self.installer_session()?;
        self.agent
            .stage_installer(&session, package, reader)
            .map_err(Into::into)
    }

    pub fn begin_installation(
        &self,
        staged: &StagedInstaller,
    ) -> Result<PreparedInstallation, LaunchError> {
        let (_, session) = self.installer_session()?;
        self.agent
            .begin_installation(&session, staged)
            .map_err(Into::into)
    }

    /// Attaches only the app-focused appliance surface for interactive setup.
    /// It cannot silently normalize to the administrator recovery desktop.
    pub fn present_installation(
        &self,
        prepared: &PreparedInstallation,
    ) -> Result<RemoteAppProcess, LaunchError> {
        if prepared.presentation.presentation != crate::ApplicationPresentation::AppFocusedSpice
            || prepared.presentation.remote_app_alias != prepared.operation_id.as_str()
        {
            return Err(LaunchError::PublicationChanged);
        }
        let lease_id = format!("install-{}", prepared.operation_id.as_str());
        self.launcher
            .launch(&prepared.presentation, &BrokeredLaunch::deny_all(lease_id))
            .map_err(Into::into)
    }

    pub fn installation_status(
        &self,
        operation: &InstallationOperationId,
    ) -> Result<InstallationStatus, LaunchError> {
        let (_, session) = self.installer_session()?;
        self.agent
            .installation_status(&session, operation)
            .map_err(Into::into)
    }

    pub fn publish_candidate(
        &self,
        operation: &InstallationOperationId,
        candidate_id: &str,
        application_id: &str,
        display_name: &str,
        expected_generation: u64,
    ) -> Result<PublicationResult, LaunchError> {
        let (state, session) = self.installer_session()?;
        let published = self.agent.publish_candidate(
            &session,
            operation,
            candidate_id,
            application_id,
            display_name,
            expected_generation,
        )?;
        self.accept_publication_catalog(state, &session, &published)?;
        Ok(published)
    }

    pub fn remove_publication(
        &self,
        application_id: &str,
        expected_generation: u64,
    ) -> Result<PublicationResult, LaunchError> {
        let (state, session) = self.installer_session()?;
        let removed =
            self.agent
                .remove_publication(&session, application_id, expected_generation)?;
        self.accept_publication_catalog(state, &session, &removed)?;
        Ok(removed)
    }

    pub fn begin_uninstallation(
        &self,
        application_id: &str,
    ) -> Result<PreparedUninstallation, LaunchError> {
        let (_, session) = self.installer_session()?;
        self.agent
            .begin_uninstallation(&session, application_id)
            .map_err(Into::into)
    }

    pub fn present_uninstallation(
        &self,
        prepared: &PreparedUninstallation,
    ) -> Result<RemoteAppProcess, LaunchError> {
        if prepared.presentation.presentation != crate::ApplicationPresentation::AppFocusedSpice
            || prepared.presentation.remote_app_alias != prepared.operation_id.as_str()
        {
            return Err(LaunchError::PublicationChanged);
        }
        let lease_id = format!("uninstall-{}", prepared.operation_id.as_str());
        self.launcher
            .launch(&prepared.presentation, &BrokeredLaunch::deny_all(lease_id))
            .map_err(Into::into)
    }

    pub fn uninstallation_status(
        &self,
        operation: &UninstallationOperationId,
    ) -> Result<UninstallationStatus, LaunchError> {
        let (state, session) = self.installer_session()?;
        let status = self.agent.uninstallation_status(&session, operation)?;
        if let Some(generation) = status.catalog_generation {
            let catalog = self.agent.catalog(&session)?;
            if catalog.generation != generation {
                return Err(LaunchError::PublicationChanged);
            }
            self.accept_catalog(state, &catalog)?;
        }
        Ok(status)
    }

    /// Starts one allow-listed `RemoteApp`. The capability lease is revoked on
    /// every error after it is created and when `FreeRDP` exits normally.
    pub fn launch(&self, request: &LaunchRequest) -> Result<RunningRemoteApp, LaunchError> {
        validate_request(request)?;
        let state = self.ensure_ready()?;
        let session = self.agent.wait_ready(self.agent_timeout)?;
        validate_health(&state, &session.health)?;

        let catalog = self.agent.catalog(&session)?;
        self.accept_catalog(state, &catalog)?;
        let application = catalog
            .application(&request.application_id)
            .ok_or_else(|| LaunchError::ApplicationNotPublished(request.application_id.clone()))?;

        let brokered = self.capabilities.materialize(request)?;
        let result = (|| {
            brokered.validate()?;
            if brokered.network && !request.grants.network {
                return Err(LaunchError::BrokerEscalated("network"));
            }
            if brokered.clipboard && !request.grants.clipboard {
                return Err(LaunchError::BrokerEscalated("clipboard"));
            }
            if brokered.microphone && !request.grants.microphone {
                return Err(LaunchError::BrokerEscalated("microphone"));
            }
            let granted_documents = request
                .grants
                .portal_documents
                .iter()
                .map(|document| (document.handle.as_str(), document.access))
                .collect::<std::collections::HashMap<_, _>>();
            if brokered.shares.iter().any(|share| {
                granted_documents
                    .get(share.portal_document_id.as_str())
                    .is_none_or(|access| share.read_only != access.is_read_only())
            }) {
                return Err(LaunchError::BrokerEscalated("files"));
            }

            let guest = self.agent.prepare_launch(
                &session,
                &request.application_id,
                brokered.network_binding.as_ref(),
            )?;
            if guest.remote_app_alias != application.remote_app_alias {
                return Err(LaunchError::PublicationChanged);
            }
            let process = self.launcher.launch(&guest, &brokered)?;
            Ok(RunningRemoteApp {
                application_id: request.application_id.clone(),
                workspace_id: request.workspace_id.clone(),
                lease_id: brokered.lease_id.clone(),
                process,
            })
        })();
        self.capabilities.revoke(&brokered.lease_id);
        result
    }

    fn accept_catalog(&self, state: ApplianceState, catalog: &Catalog) -> Result<(), LaunchError> {
        catalog.validate()?;
        if catalog.generation < state.catalog_generation {
            return Err(LaunchError::CatalogRollback {
                stored: state.catalog_generation,
                received: catalog.generation,
            });
        }
        let mut accepted = state;
        accepted.catalog_generation = catalog.generation;
        accepted.cached_catalog = Some(catalog.clone());
        self.state.save(&accepted)?;
        Ok(())
    }

    fn installer_session(&self) -> Result<(ApplianceState, crate::GuestSession), LaunchError> {
        let state = self.ensure_ready()?;
        let session = self.agent.wait_ready(self.agent_timeout)?;
        validate_health(&state, &session.health)?;
        Ok((state, session))
    }

    fn accept_publication_catalog(
        &self,
        state: ApplianceState,
        session: &crate::GuestSession,
        result: &PublicationResult,
    ) -> Result<(), LaunchError> {
        let catalog = self.agent.catalog(session)?;
        if catalog.generation != result.catalog_generation {
            return Err(LaunchError::PublicationChanged);
        }
        self.accept_catalog(state, &catalog)
    }

    fn ready_state(&self) -> Result<ApplianceState, LaunchError> {
        let state = self.state.load()?;
        if !state.can_launch() {
            return Err(LaunchError::NotReady(state));
        }
        Ok(state)
    }

    /// Turns a prepared guest into the one launchable appliance only after the
    /// authenticated bridge proves its protocol, licence state, and monotonic
    /// catalogue generation.  This is intentionally reached by an explicit
    /// application launch, never by the manager's availability refresh.
    fn ensure_ready(&self) -> Result<ApplianceState, LaunchError> {
        let stored = self.state.load()?;
        if stored.can_launch() {
            self.ensure_running()?;
            return Ok(stored);
        }
        if !matches!(
            stored.phase,
            crate::AppliancePhase::Installing { .. } | crate::AppliancePhase::AwaitingGuestBridge
        ) {
            return Err(LaunchError::NotReady(stored));
        }
        self.ensure_running()?;
        let session = self.agent.wait_ready(self.agent_timeout)?;
        validate_health(&stored, &session.health)?;
        let ready = ApplianceState {
            schema_version: stored.schema_version,
            phase: crate::AppliancePhase::Ready,
            license: session.health.license.clone(),
            catalog_generation: session.health.catalog_generation,
            cached_catalog: None,
        };
        // The tools ISO contains the one-time enrollment key. Do not mark the
        // appliance launchable until both setup images have been removed from
        // the live and persistent domain configuration.
        self.backend.retire_installation_media(&self.config)?;
        self.state.save(&ready)?;
        Ok(ready)
    }

    fn ensure_running(&self) -> Result<(), LaunchError> {
        self.backend.ensure_singleton(&self.config)?;
        match self.backend.state(&self.config)? {
            DomainState::Running => Ok(()),
            DomainState::ShutOff | DomainState::Crashed => {
                self.backend.start(&self.config).map_err(Into::into)
            }
            DomainState::Absent => Err(LaunchError::DomainDisappeared),
            DomainState::Paused
            | DomainState::Suspended
            | DomainState::Starting
            | DomainState::ShuttingDown
            | DomainState::Other => Err(LaunchError::RecoveryRequired),
        }
    }
}

fn validate_health(
    stored: &ApplianceState,
    health: &crate::AgentHealth,
) -> Result<(), LaunchError> {
    if health.protocol_major != HOST_PROTOCOL_MAJOR {
        return Err(AgentError::ProtocolMismatch {
            expected: HOST_PROTOCOL_MAJOR,
            actual: health.protocol_major,
        }
        .into());
    }
    if health.license == crate::LicenseState::Unknown {
        return Err(LaunchError::GuestLicenseUnknown);
    }
    if health.catalog_generation < stored.catalog_generation {
        return Err(LaunchError::CatalogRollback {
            stored: stored.catalog_generation,
            received: health.catalog_generation,
        });
    }
    Ok(())
}

fn validate_request(request: &LaunchRequest) -> Result<(), LaunchError> {
    if request.workspace_id.trim().is_empty() {
        return Err(LaunchError::InvalidRequest("workspace id"));
    }
    if request.application_id.is_empty()
        || !request
            .application_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(LaunchError::InvalidRequest("application id"));
    }
    let mut handles = HashSet::new();
    if request.grants.portal_documents.iter().any(|document| {
        document.handle.trim().is_empty()
            || document.handle.chars().any(char::is_control)
            || !handles.insert(document.handle.as_str())
    }) {
        return Err(LaunchError::InvalidRequest("portal document id"));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("the Windows appliance is not ready: {0:?}")]
    NotReady(ApplianceState),
    #[error("invalid launch request field {0}")]
    InvalidRequest(&'static str),
    #[error("application {0:?} was not published by the authenticated guest bridge")]
    ApplicationNotPublished(String),
    #[error("the publication changed while the application was being launched")]
    PublicationChanged,
    #[error("guest catalogue rolled back from generation {stored} to {received}")]
    CatalogRollback { stored: u64, received: u64 },
    #[error("the guest has not reported its Windows licensing state")]
    GuestLicenseUnknown,
    #[error("capability broker returned access broader than the Tilo grant: {0}")]
    BrokerEscalated(&'static str),
    #[error("the appliance domain disappeared")]
    DomainDisappeared,
    #[error("the appliance needs the explicit administrator recovery flow")]
    RecoveryRequired,
    #[error(transparent)]
    Backend(#[from] VmBackendError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Provisioning(#[from] ProvisioningError),
    #[error(transparent)]
    RemoteApp(#[from] RemoteAppError),
    #[error("capability broker failed: {0}")]
    CapabilityBroker(String),
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;

    use tempfile::TempDir;

    use crate::{
        AgentHealth, Association, AssociationKind, GuestLaunch, GuestSession, LicenseState,
        PublishedApplication, RdpEndpoint, Secret,
    };

    use super::*;

    struct Backend(DomainState);

    impl VmBackend for Backend {
        fn ensure_singleton(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            Ok(())
        }
        fn state(&self, _: &ApplianceConfig) -> Result<DomainState, VmBackendError> {
            Ok(self.0)
        }
        fn start(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            Ok(())
        }
        fn retire_installation_media(&self, _: &ApplianceConfig) -> Result<(), VmBackendError> {
            Ok(())
        }
    }

    struct Agent {
        available: bool,
        alias_on_launch: String,
    }

    impl GuestAgent for Agent {
        fn wait_ready(&self, _: Duration) -> Result<GuestSession, AgentError> {
            if !self.available {
                return Err(AgentError::Unavailable);
            }
            Ok(GuestSession {
                id: "session".to_string(),
                health: AgentHealth {
                    protocol_major: 1,
                    guest_boot_id: "boot".to_string(),
                    catalog_generation: 2,
                    license: LicenseState::DigitallyActivated,
                },
            })
        }

        fn catalog(&self, _: &GuestSession) -> Result<Catalog, AgentError> {
            Ok(Catalog {
                generation: 2,
                applications: vec![PublishedApplication {
                    id: "word".to_string(),
                    display_name: "Word".to_string(),
                    remote_app_alias: "tilo-word".to_string(),
                    icon_sha256: None,
                    associations: vec![Association {
                        kind: AssociationKind::Extension,
                        value: ".docx".to_string(),
                    }],
                    uninstall_available: false,
                }],
            })
        }

        fn prepare_launch(
            &self,
            _: &GuestSession,
            _: &str,
            _: Option<&crate::GuestNetworkBinding>,
        ) -> Result<GuestLaunch, AgentError> {
            Ok(GuestLaunch {
                endpoint: RdpEndpoint {
                    address: (IpAddr::V4(Ipv4Addr::LOCALHOST), 3389).into(),
                    username: "tilo".to_string(),
                    certificate_sha256: "a".repeat(64),
                },
                password: Secret::new(b"secret".to_vec()),
                remote_app_alias: self.alias_on_launch.clone(),
                launch_nonce: "nonce".to_string(),
                presentation: crate::ApplicationPresentation::NativeRail,
            })
        }
    }

    struct Broker {
        launch: BrokeredLaunch,
        revoked: Mutex<Vec<String>>,
    }

    impl CapabilityBroker for Broker {
        fn materialize(&self, _: &LaunchRequest) -> Result<BrokeredLaunch, LaunchError> {
            Ok(self.launch.clone())
        }

        fn revoke(&self, lease_id: &str) {
            self.revoked.lock().unwrap().push(lease_id.to_string());
        }
    }

    struct Launcher;

    impl RemoteAppLauncher for Launcher {
        fn launch(
            &self,
            _: &GuestLaunch,
            _: &BrokeredLaunch,
        ) -> Result<RemoteAppProcess, RemoteAppError> {
            Ok(RemoteAppProcess { exit_code: Some(0) })
        }
    }

    fn fixture(
        directory: &TempDir,
        agent: Agent,
        brokered: BrokeredLaunch,
    ) -> WindowsAppliance<Backend, Agent, Broker, Launcher> {
        let store = ProvisioningStore::new(directory.path().join("state.json"));
        store
            .save(&ApplianceState {
                schema_version: 1,
                phase: crate::AppliancePhase::Ready,
                license: LicenseState::DigitallyActivated,
                catalog_generation: 1,
                cached_catalog: None,
            })
            .unwrap();
        WindowsAppliance::new(
            ApplianceConfig::recommended(
                "01234567-89ab-cdef-0123-456789abcdef",
                "/var/lib/tilo/windows/system.qcow2",
                "/var/lib/tilo/windows/nvram.fd",
            ),
            store,
            Backend(DomainState::Running),
            agent,
            Broker {
                launch: brokered,
                revoked: Mutex::new(Vec::new()),
            },
            Launcher,
        )
    }

    fn request() -> LaunchRequest {
        LaunchRequest {
            workspace_id: "workspace-1".to_string(),
            application_id: "word".to_string(),
            grants: VmLaunchGrants::default(),
        }
    }

    #[test]
    fn appliance_launches_require_an_available_guest_and_published_unchanged_apps() {
        // Phase `unavailable_guest_bridge_fails_closed`.
        {
            let directory = TempDir::new().unwrap();
            let appliance = fixture(
                &directory,
                Agent {
                    available: false,
                    alias_on_launch: "tilo-word".to_string(),
                },
                BrokeredLaunch::deny_all("lease"),
            );
            assert!(matches!(
                appliance.launch(&request()),
                Err(LaunchError::Agent(AgentError::Unavailable))
            ));
        }
        // Phase `unpublished_application_cannot_be_launched`.
        {
            let directory = TempDir::new().unwrap();
            let appliance = fixture(
                &directory,
                Agent {
                    available: true,
                    alias_on_launch: "tilo-word".to_string(),
                },
                BrokeredLaunch::deny_all("lease"),
            );
            let mut bad = request();
            bad.application_id = "arbitrary-exe".to_string();
            assert!(matches!(
                appliance.launch(&bad),
                Err(LaunchError::ApplicationNotPublished(_))
            ));
        }
        // Phase `changed_publication_alias_fails_and_revokes_lease`.
        {
            let directory = TempDir::new().unwrap();
            let appliance = fixture(
                &directory,
                Agent {
                    available: true,
                    alias_on_launch: "tilo-different".to_string(),
                },
                BrokeredLaunch::deny_all("lease"),
            );
            assert!(matches!(
                appliance.launch(&request()),
                Err(LaunchError::PublicationChanged)
            ));
            assert_eq!(
                appliance.capabilities.revoked.lock().unwrap().as_slice(),
                ["lease"]
            );
        }
    }

    #[test]
    fn brokers_cannot_widen_portal_documents_handles_or_clipboard() {
        // Phase `broker_cannot_upgrade_a_read_only_portal_document_to_a_writable_drive`.
        {
            let directory = TempDir::new().unwrap();
            let appliance = fixture(
                &directory,
                Agent {
                    available: true,
                    alias_on_launch: "tilo-word".to_string(),
                },
                BrokeredLaunch {
                    lease_id: "lease".to_string(),
                    network: false,
                    network_binding: None,
                    clipboard: false,
                    microphone: false,
                    shares: vec![ShareBinding {
                        drive_name: "TILO1".to_string(),
                        portal_document_id: "portal-document".to_string(),
                        host_path: crate::portal_lease_root().join("lease/document"),
                        read_only: false,
                    }],
                },
            );
            let mut request = request();
            request.grants.portal_documents = vec![PortalDocumentGrant {
                handle: "portal-document".to_string(),
                access: PortalDocumentAccess::ReadOnly,
            }];
            assert!(matches!(
                appliance.launch(&request),
                Err(LaunchError::BrokerEscalated("files"))
            ));
        }
        // Phase `duplicate_portal_handles_are_rejected_before_any_guest_connection`.
        {
            let directory = TempDir::new().unwrap();
            let appliance = fixture(
                &directory,
                Agent {
                    available: true,
                    alias_on_launch: "tilo-word".to_string(),
                },
                BrokeredLaunch::deny_all("lease"),
            );
            let mut request = request();
            request.grants.portal_documents = vec![
                PortalDocumentGrant {
                    handle: "portal-document".to_string(),
                    access: PortalDocumentAccess::ReadOnly,
                },
                PortalDocumentGrant {
                    handle: "portal-document".to_string(),
                    access: PortalDocumentAccess::ReadWrite,
                },
            ];
            assert!(matches!(
                appliance.launch(&request),
                Err(LaunchError::InvalidRequest("portal document id"))
            ));
        }
        // Phase `a_broker_cannot_enable_ungranted_clipboard`.
        {
            let directory = TempDir::new().unwrap();
            let appliance = fixture(
                &directory,
                Agent {
                    available: true,
                    alias_on_launch: "tilo-word".to_string(),
                },
                BrokeredLaunch {
                    lease_id: "lease".to_string(),
                    network: false,
                    network_binding: None,
                    clipboard: true,
                    microphone: false,
                    shares: Vec::new(),
                },
            );
            assert!(matches!(
                appliance.launch(&request()),
                Err(LaunchError::BrokerEscalated("clipboard"))
            ));
            assert_eq!(
                appliance.capabilities.revoked.lock().unwrap().as_slice(),
                ["lease"]
            );
        }
    }

    #[test]
    fn the_passive_catalogue_cache_is_authenticated_and_never_wakes_the_guest() {
        // Phase `authenticated_catalogue_is_cached_for_passive_manager_enumeration`.
        {
            let directory = TempDir::new().unwrap();
            let appliance = fixture(
                &directory,
                Agent {
                    available: true,
                    alias_on_launch: "tilo-word".to_string(),
                },
                BrokeredLaunch::deny_all("lease"),
            );
            let fresh = appliance.catalog().unwrap();
            assert_eq!(fresh.generation, 2);

            let cached = appliance.cached_catalog().unwrap().unwrap();
            assert_eq!(cached, fresh);
            assert_eq!(appliance.state.load().unwrap().catalog_generation, 2);
        }
        // Phase `passive_catalogue_cache_never_contacts_an_unavailable_guest`.
        {
            let directory = TempDir::new().unwrap();
            let appliance = fixture(
                &directory,
                Agent {
                    available: false,
                    alias_on_launch: "tilo-word".to_string(),
                },
                BrokeredLaunch::deny_all("lease"),
            );
            let cached = Catalog {
                generation: 1,
                applications: vec![PublishedApplication {
                    id: "word".to_string(),
                    display_name: "Word".to_string(),
                    remote_app_alias: "tilo-word".to_string(),
                    icon_sha256: None,
                    associations: Vec::new(),
                    uninstall_available: false,
                }],
            };
            appliance
                .state
                .save(&ApplianceState {
                    schema_version: 1,
                    phase: crate::AppliancePhase::Ready,
                    license: LicenseState::DigitallyActivated,
                    catalog_generation: 1,
                    cached_catalog: Some(cached.clone()),
                })
                .unwrap();

            assert_eq!(appliance.cached_catalog().unwrap(), Some(cached));
        }
    }
}
