//! The session's system information service (`tilo_system_stats`).
//!
//! The compositor reads the kernel for every app allowed to see the machine,
//! so no app sandbox ever needs the host's `/proc`. It serves the launcher's
//! authenticated connections at `<services root>/system-stats.sock` from the
//! session's one [`Hub`], the same one the desk's System monitor widget
//! reads, and answers an app's request for a grant with the ordinary
//! permission prompt: the person's answer is kept as their other portal
//! choices are (once, for this session, or always, per room and app).

use std::sync::Arc;

use tilo_system_stats::Grant;
use tilo_system_stats::hub::Hub;
use tilo_system_stats::protocol::Handoff;
use tilo_system_stats::server::{Prompter, Server};

use tilo_desktop_model::capability::{Capability, GRANT_ONCE, GrantScope};

use crate::portal::{
    CapabilityKind, GrantStoreHandle, PortalCaller, PromptOutcome, PromptQueueHandle,
};

/// The permission prompt, as the service asks it.
pub struct PortalPrompter {
    pub grants: GrantStoreHandle,
    pub prompts: PromptQueueHandle,
    /// Wakes the compositor loop to show a queued prompt.
    pub wake: crate::session_runtime::Wake,
}

fn kind(grant: Grant) -> CapabilityKind {
    match grant {
        Grant::SystemInfo => CapabilityKind::SystemInfo,
        Grant::Processes => CapabilityKind::Processes,
        Grant::CommandLines => CapabilityKind::ProcessCommands,
    }
}

fn capability(grant: Grant, scope: GrantScope) -> Capability {
    match grant {
        Grant::SystemInfo => Capability::SystemInfo { scope },
        Grant::Processes => Capability::Processes { scope },
        Grant::CommandLines => Capability::ProcessCommands { scope },
    }
}

impl PortalPrompter {
    /// Who asks: the app, in the room its launcher named. An app launched
    /// into no room has no one to attribute a choice to and is not asked.
    fn caller(&self, handoff: &Handoff) -> Option<PortalCaller> {
        let room = handoff.workspace_id.as_deref()?;
        self.grants.caller_in_room(room, &handoff.app_id)
    }
}

impl Prompter for PortalPrompter {
    fn granted(&self, handoff: &Handoff, grant: Grant) -> bool {
        self.caller(handoff)
            .is_some_and(|caller| self.grants.lookup(&caller, kind(grant)).is_some())
    }

    fn ask(&self, handoff: &Handoff, grant: Grant) -> bool {
        let Some(caller) = self.caller(handoff) else {
            return false;
        };
        let answer = self
            .prompts
            .enqueue(caller.clone(), capability(grant, GRANT_ONCE));
        (self.wake)();
        // This connection's own thread waits; the service and the
        // compositor go on. A prompt that is withdrawn answers no.
        let outcome = pollster::block_on(answer).unwrap_or(PromptOutcome::Deny);
        if let Some(scope) = outcome.to_scope() {
            // "This session" and "Always" are remembered as the portal's
            // choices are; "Allow" lasts as long as this connection.
            self.grants
                .insert_portal_consent(&caller, &capability(grant, scope));
        }
        outcome.is_allowed()
    }
}

/// Serve the session's hub to the apps the launcher connects.
pub fn start(prompter: Arc<dyn Prompter>) -> Option<Server> {
    if let Err(error) = launcher::services::ensure_services_root() {
        tracing::warn!(%error, "system information service unavailable");
        return None;
    }
    let path = launcher::services::socket_for(tilo_system_stats::SERVICE);
    match Server::bind(&path, Hub::shared().clone(), prompter) {
        Ok(server) => {
            tracing::info!("serving system information to apps");
            Some(server)
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "system information service unavailable");
            None
        }
    }
}
