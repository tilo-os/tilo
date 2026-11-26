//! Where a host's Browser sessions start, as the daemon hands them to the host: the
//! workspaces' devices that run Linux take each session's turn
//! (`tilo_runtime`'s `LinuxDevices`), and the host places, routes and counts the windows
//! ([`crate::device::keep::hosted_browser`]).

use tilo_action_services::linux_apps::AppLaunch;
use tilo_action_services::server_account::Refusal;
use tilo_server_protocol::hosted::Material;

/// The sessions a host's Browser starts through, and the account that pays for their
/// machines.
pub struct Sessions {
    pub sessions: std::sync::Arc<dyn BrowserSessions>,
    pub account: String,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

/// A browser session that is up, and the window it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The device the workspace's VM joined as: the one its placement's ticket was certified
    /// for, since one VM joins with one ticket.
    pub device: [u8; 32],
    /// The provider token the window publishes under (`browser@42`): what a
    /// page action names in place of the app.
    pub provider: String,
}

/// What a session in the workspace's VM answers once it is up: the provider token its window
/// publishes under (`browser@42`). The device it runs as is the one its ticket certified, which
/// the host knows already.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Started {
    pub provider: String,
}

/// The service that starts and stops a workspace's browser sessions.
///
/// The workspace's devices that run Linux implement this (`tilo_runtime`'s
/// `LinuxDevices`): the one that takes the turn starts
/// the session in the workspace's VM with the [`Material`], and it answers once
/// the session reports its window live.
pub trait BrowserSessions: Send + Sync {
    /// Start the workspace's browser session with `material`, and answer its
    /// window once it is live.
    ///
    /// # Errors
    ///
    /// The words to refuse the open with: no host had room, or the host
    /// would not start it.
    fn start(&self, material: &Material) -> Result<Started, String>;

    /// The workspace's browser session stopped, or must: its device is
    /// forgotten and its window no longer exists.
    fn stop(&self, workspace: &str);

    /// Start `launch` in the workspace's Linux apps session, starting that
    /// session with `material` when none runs, and answer once it is up and
    /// was told to start the app. The app's windows join the shell as they
    /// map.
    ///
    /// # Errors
    ///
    /// The words to refuse the open with, and the plans' typed refusal when
    /// the account may not run Linux.
    fn start_app(&self, material: &Material, launch: &AppLaunch) -> Result<Started, AppRefusal> {
        let _ = (material, launch);
        Err(AppRefusal::Words(
            "this host starts no Linux apps session".to_owned(),
        ))
    }

    /// The workspace's Linux apps session stopped, or must.
    fn stop_apps(&self, workspace: &str) {
        let _ = workspace;
    }

    /// The workspace's VM device was certified again, with `cert`, before its certificate ended:
    /// hand it to the VM, whose daemon holds it from then on. Sessions whose VM takes no
    /// certificate from its host ignore it.
    ///
    /// # Errors
    ///
    /// The VM was not reached, or did not take it.
    fn renewed(
        &self,
        workspace: &str,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        let _ = (workspace, cert);
        Ok(())
    }

    /// Ask the hosts that run sessions as `actions`'s device, from `endpoint`:
    /// this host's own, once it runs, holding `workspaces` read on `runtime`.
    /// Sessions that need no host to ask ignore it.
    fn ask_from(
        &self,
        actions: tilo_peer_state::actions::ActionService,
        endpoint: iroh::Endpoint,
        workspaces: tilo_workspace_service::workspaces::Workspaces,
        runtime: tokio::runtime::Handle,
    ) {
        let _ = (actions, endpoint, workspaces, runtime);
    }
}

/// Why a workspace's Linux app did not open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppRefusal {
    /// The account's plan does not let it run Linux: the plans' own typed
    /// refusal, which says what would let it through.
    Refused(Refusal),
    /// Anything else, in words for whoever opened it.
    Words(String),
}

impl std::fmt::Display for AppRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => f.write_str(&refusal.message),
            Self::Words(words) => f.write_str(words),
        }
    }
}
