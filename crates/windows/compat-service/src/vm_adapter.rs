//! Live adapter from qOS's compatibility service to the one Windows appliance.
//!
//! The adapter owns no Windows policy.  In particular it cannot turn a host
//! path, a catalogue recipe, or a manager request into an RDP drive.  The
//! existing privileged portal/capability service supplies an already-resolved
//! [`windows_vm::VmLaunchGrants`] projection; `windows-vm` then materialises
//! and revokes the corresponding lease around the `RemoteApp` process.

use compat_catalogue::Layer;
use launcher::capabilities::CapabilityPlan;

use crate::{VmAvailability, VmDelegate, VmRouteRequest};

/// Privileged qOS authority for the capability projection used by the Windows
/// appliance.  It is intentionally an injected adapter rather than a second
/// permission database: implementations use the ordinary portal document
/// handles and workspace decision records already owned by qOS.
///
/// A portal implementation may return opaque `portal_documents`, clipboard,
/// or a network lease only when it has materialised and can later revoke that
/// exact resource.  Raw host paths are never part of this API.
pub trait VmPortalAuthority: Send + Sync {
    fn grants_for(
        &self,
        app_id: &str,
        workspace_id: &str,
        capabilities: &CapabilityPlan,
    ) -> Result<windows_vm::VmLaunchGrants, windows_vm::LaunchError>;
}

/// Safe bootstrap authority for deployments where the privileged qOS portal
/// bridge has not been installed yet.  `RemoteApp` itself still works, but it
/// receives no files, clipboard, microphone, or egress.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyAllVmPortalAuthority;

impl VmPortalAuthority for DenyAllVmPortalAuthority {
    fn grants_for(
        &self,
        _app_id: &str,
        _workspace_id: &str,
        _capabilities: &CapabilityPlan,
    ) -> Result<windows_vm::VmLaunchGrants, windows_vm::LaunchError> {
        Ok(windows_vm::VmLaunchGrants::default())
    }
}

/// Concrete delegate installed in qOS's privileged compatibility host.
///
/// `WindowsAppliance` provides the singleton libvirt backend, authenticated
/// guest bridge, catalogue validation, selected RAIL/SPICE presentation, and
/// short-lived resource revocation.  This type supplies the missing service
/// glue: only the catalogue-selected published application id crosses the
/// boundary and only the normal qOS portal may project grants.
pub struct LiveVmDelegate<B, A, C, L, P> {
    appliance: windows_vm::WindowsAppliance<B, A, C, L>,
    portal: P,
    /// The host surface chosen by the authenticated guest bridge adapter.
    /// It is descriptive for availability; `WindowsAppliance::launch` still
    /// verifies the guest's actual response before a viewer is started.
    presentation: windows_vm::ApplicationPresentation,
}

impl<B, A, C, L, P> LiveVmDelegate<B, A, C, L, P> {
    #[must_use]
    pub fn new(
        appliance: windows_vm::WindowsAppliance<B, A, C, L>,
        portal: P,
        presentation: windows_vm::ApplicationPresentation,
    ) -> Self {
        Self {
            appliance,
            portal,
            presentation,
        }
    }

    #[must_use]
    pub fn into_parts(self) -> (windows_vm::WindowsAppliance<B, A, C, L>, P) {
        (self.appliance, self.portal)
    }
}

impl<B, A, C, L, P> VmDelegate for LiveVmDelegate<B, A, C, L, P>
where
    B: windows_vm::VmBackend,
    A: windows_vm::GuestAgent,
    C: windows_vm::CapabilityBroker,
    L: windows_vm::RemoteAppLauncher,
    P: VmPortalAuthority,
{
    fn availability(&self) -> VmAvailability {
        match self.appliance.launch_readiness() {
            Ok(_) => VmAvailability::ready(format!(
                "Ready for an explicit fallback launch using {}.",
                self.presentation
            )),
            Err(error) => VmAvailability::unavailable(error.to_string()),
        }
    }

    fn launch(
        &self,
        request: VmRouteRequest<'_>,
    ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
        // The catalogue itself is the only source allowed to select the VM.
        // This prevents a manager-side route override from using the VM as an
        // arbitrary executable runner.
        if request.route.runtime.layer != Layer::Vm {
            return Err(windows_vm::LaunchError::InvalidRequest("VM route"));
        }
        let grants = self.portal.grants_for(
            request.app_id,
            request.workspace_id,
            request.resolved_capabilities,
        )?;
        validate_portal_projection(&grants, request.resolved_capabilities)?;
        self.appliance.launch(&windows_vm::LaunchRequest {
            workspace_id: request.workspace_id.to_string(),
            // `published_application_id` is a server-owned registered
            // program id. `WindowsAppliance` additionally requires it to be
            // present in the authenticated guest catalogue before launch.
            application_id: request.published_application_id.to_string(),
            grants,
        })
    }
}

fn validate_portal_projection(
    grants: &windows_vm::VmLaunchGrants,
    capabilities: &CapabilityPlan,
) -> Result<(), windows_vm::LaunchError> {
    if grants.network && !capabilities.allow_network {
        return Err(windows_vm::LaunchError::BrokerEscalated("network"));
    }
    if grants.microphone
        && !capabilities
            .devices
            .iter()
            .any(|device| device == "microphone")
    {
        return Err(windows_vm::LaunchError::BrokerEscalated("microphone"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_portal_projection_is_fully_denied() {
        let grants = DenyAllVmPortalAuthority
            .grants_for("app", "workspace", &CapabilityPlan::empty())
            .unwrap();
        assert!(!grants.network);
        assert!(!grants.clipboard);
        assert!(!grants.microphone);
        assert!(grants.portal_documents.is_empty());
    }

    #[test]
    fn a_portal_cannot_project_ungranted_network_or_microphone() {
        let result = validate_portal_projection(
            &windows_vm::VmLaunchGrants {
                network: true,
                clipboard: false,
                microphone: true,
                portal_documents: Vec::new(),
            },
            &CapabilityPlan::empty(),
        );
        assert!(matches!(
            result,
            Err(windows_vm::LaunchError::BrokerEscalated("network"))
        ));

        let result = validate_portal_projection(
            &windows_vm::VmLaunchGrants {
                network: false,
                clipboard: false,
                microphone: true,
                portal_documents: Vec::new(),
            },
            &CapabilityPlan::empty(),
        );
        assert!(matches!(
            result,
            Err(windows_vm::LaunchError::BrokerEscalated("microphone"))
        ));
    }
}
