//! Compositor-side host for the privileged broker.
//!
//! The [broker](qos_privileged_broker) is the trusted helper that performs a
//! privileged system operation on a sandboxed app's behalf. It runs outside
//! every app sandbox — and the compositor, which owns the workspace tree and
//! the consent prompts, is where "outside the sandbox" lives. This module is
//! the seam: it supplies the broker's third gate, **consent**, straight out
//! of the compositor's own capability model.
//!
//! ## Consent is the compositor's existing grant flow
//!
//! A privileged capability is stored, prompted, shown and revoked exactly
//! like a camera or network grant — as a [`Capability::Privileged`] whose
//! [`CapOverride`](crate::shell::state::CapOverride) lives on
//! `Scope::Device("privileged:<name>")`. So the broker's consent question is
//! the very question [`crate::portal::GrantStoreHandle::permits`] already
//! answers: "does this app hold `Admin` on this scope, in this workspace,
//! right now?" [`WorkspaceConsent`] is that call and nothing more — there is
//! no second permission store for privileged operations, so a grant the
//! Permissions tab shows and a grant the broker honours cannot drift apart.
//!
//! ## What remains, stated plainly
//!
//! This wires the **standing-grant** path: an app that already holds consent
//! is served, one that does not is refused. Raising the consent *prompt* when
//! no standing grant exists is the compositor's async prompt pump
//! ([`crate::portal::PromptQueueHandle`]) enqueuing a
//! [`Capability::Privileged`] and, on Allow, [`GrantStoreHandle::insert`]ing
//! it — the same pump every other portal prompt already uses. Until that
//! enqueue-on-miss is added to the accept loop, a privileged operation
//! succeeds only after the user has granted the capability (from the
//! Permissions tab, or a future prompt); it never succeeds without consent.

use qos_privileged_broker::{ConsentOracle, PrivilegedCapability};

use crate::portal::{GrantStoreHandle, PortalCaller};
use crate::shell::state::{ResourceRef, Scope, Verb};

/// The broker's consent oracle, backed by a workspace's capability list.
///
/// Built per accepted broker connection from the connection's resolved
/// identity (`caller`) and the shared workspace tree (`grants`). Reading the
/// identity from the connection, never from the request, is what stops an app
/// speaking for another.
pub struct WorkspaceConsent {
    grants: GrantStoreHandle,
    caller: PortalCaller,
}

impl WorkspaceConsent {
    /// Build a consent oracle for `caller` (the app + workspace the broker
    /// connection belongs to) over the shared workspace tree.
    pub fn new(grants: GrantStoreHandle, caller: PortalCaller) -> Self {
        Self { grants, caller }
    }
}

impl ConsentOracle for WorkspaceConsent {
    fn consents(&self, app_id: &str, capability: PrivilegedCapability) -> bool {
        // The broker passes the connection's identity through; it must be the
        // one this oracle was built for. A mismatch is a bug or an attempt to
        // speak for another app — either way, deny.
        if app_id != self.caller.app_id.as_str() {
            return false;
        }
        // A privileged capability maps to a `privileged:<name>` device scope;
        // ask the workspace for `Admin` on it, the same call every portal
        // check makes. Anything that is not that scope shape is not a
        // privileged consent question and denies.
        let resource = match capability.scope() {
            Scope::Device(device) => ResourceRef::Device(device),
            _ => return false,
        };
        self.grants.permits(&self.caller, &resource, Verb::Admin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Capability, GRANT_ALWAYS, Workspace};

    /// A workspace with `app` granted the privileged `capability`, exactly as
    /// the consent prompt / Permissions tab would record it.
    fn workspace_granting(app: &str, capability: &str) -> Workspace {
        let mut ws = Workspace::new(1.into(), "test");
        ws.grant_app(
            app,
            &Capability::Privileged {
                capability: capability.to_string(),
                scope: GRANT_ALWAYS,
            },
        );
        ws
    }

    #[test]
    fn a_workspace_grant_consents_the_broker() {
        // The end-to-end shell proof: a grant made through the ordinary grant
        // flow (grant_app) is exactly what the broker's consent oracle reads
        // back as `true`.
        let ws = workspace_granting("dev.qos.firewall", "firewall-admin");
        let (grants, ws_id) = GrantStoreHandle::test_with_workspace(ws);
        let caller = PortalCaller::new(ws_id, "dev.qos.firewall");
        let consent = WorkspaceConsent::new(grants, caller);
        assert!(consent.consents("dev.qos.firewall", PrivilegedCapability::FirewallAdmin));
    }

    #[test]
    fn no_grant_denies() {
        let ws = Workspace::new(1.into(), "test");
        let (grants, ws_id) = GrantStoreHandle::test_with_workspace(ws);
        let caller = PortalCaller::new(ws_id, "dev.qos.firewall");
        let consent = WorkspaceConsent::new(grants, caller);
        assert!(!consent.consents("dev.qos.firewall", PrivilegedCapability::FirewallAdmin));
    }

    #[test]
    fn a_grant_for_one_capability_does_not_consent_another() {
        // firewall-admin granted, but the app asks to write a block device —
        // a different scope, so denied.
        let ws = workspace_granting("dev.qos.firewall", "firewall-admin");
        let (grants, ws_id) = GrantStoreHandle::test_with_workspace(ws);
        let caller = PortalCaller::new(ws_id, "dev.qos.firewall");
        let consent = WorkspaceConsent::new(grants, caller);
        assert!(!consent.consents("dev.qos.firewall", PrivilegedCapability::BlockDevice));
    }

    #[test]
    fn the_connection_identity_must_match() {
        // The grant is the firewall's; a request carrying a different app_id
        // is refused even though the workspace holds the grant.
        let ws = workspace_granting("dev.qos.firewall", "firewall-admin");
        let (grants, ws_id) = GrantStoreHandle::test_with_workspace(ws);
        let caller = PortalCaller::new(ws_id, "dev.qos.firewall");
        let consent = WorkspaceConsent::new(grants, caller);
        assert!(!consent.consents("dev.qos.evil", PrivilegedCapability::FirewallAdmin));
    }
}
