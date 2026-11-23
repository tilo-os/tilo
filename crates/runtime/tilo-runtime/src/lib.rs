//! The per-user shared runtime: one private profile whose daemon hosts the
//! identity, collaboration and content services independently launched apps
//! attach to, and an MCP projection of its action host.
//!
//! This crate owns the profile, the native endpoint and app attachment, and serves `tilo_mcp`'s
//! projection; it reaches the services only through [`RuntimeServices`].
//! [`device::services::Daemon`] hosts them without the Tilo desktop: this computer's one daemon,
//! the `tilo-runtime` executable, which serves the person's login as its device too
//! ([`device`]).

#[cfg(unix)]
use std::{
    io,
    path::Path,
    process::{Child, Command},
};

#[cfg(unix)]
mod apps;
pub mod cli;
#[cfg(unix)]
mod daemon;
#[cfg(windows)]
#[path = "daemon_windows.rs"]
mod daemon;
mod daemon_args;
#[cfg(unix)]
pub mod device;
#[cfg(unix)]
mod i18n;
#[cfg(unix)]
pub mod key_sessions;
#[cfg(unix)]
pub mod offload;
#[cfg(unix)]
mod os_search;
#[cfg(unix)]
pub mod other_devices;
#[cfg(unix)]
pub mod personal_devices;
#[cfg(unix)]
pub mod profile;
// Where this platform reaches a profile's services and starts an app against
// them: Unix sockets and inherited descriptors on Unix, and the Windows edge
// that refuses in words until named pipes and handle passing are here.
#[cfg(unix)]
#[path = "host/unix.rs"]
pub mod host;
#[cfg(windows)]
#[path = "host/windows.rs"]
pub mod host;

// Workspace admission decodes peer bytes in automerge worker processes started
// from this test binary; this serves them.
#[cfg(all(test, unix))]
tilo_automerge_isolate::test_worker!();

/// Socket names inside a profile, shared by the daemon that binds them and
/// the launcher that attaches an app to them.
pub const COLLAB_SOCKET: &str = "collab.sock";
pub const IDENTITY_SOCKET: &str = "identity.sock";
pub const BLOB_SOCKET: &str = "blobs.sock";
/// The profile's workspace files, served read only (`tilo-file-provider`).
pub const FILES_SOCKET: &str = "files.sock";

/// One running service session over a profile: the host's identity,
/// collaboration and content services.
#[cfg(unix)]
pub trait RuntimeServices: Sized {
    /// The action host MCP callers browse and invoke through.
    type ActionProvider: tilo_actions::remote::ActionProvider + Clone + Send + Sync + 'static;

    /// Start the services on `root`, naming each step to `stage` before taking
    /// it, so whoever waits can tell a slow start from a stuck one. Holds the
    /// profile exclusively until dropped.
    fn start_reporting(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self>;

    /// [`Self::start_reporting`] for app development: builds under
    /// development, which are not installed, are served too. A host without
    /// such a mode refuses it.
    /// # Errors
    /// As [`Self::start_reporting`], and when the host has no dev mode.
    fn start_in_dev_mode(root: &Path, stage: &mut dyn FnMut(&str)) -> io::Result<Self> {
        let _ = (root, stage);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this host has no dev mode",
        ))
    }

    /// The workspace every app of this profile joins.
    fn workspace(&self) -> &str;

    /// Select the fixed audience of a host-configured MCP endpoint. This
    /// never enrolls a workspace or changes grants. Hosts supporting shared workspaces
    /// must check their current certified admission and the profile actor.
    fn mcp_workspace(&self, requested: Option<&str>) -> io::Result<String> {
        let workspace = requested.unwrap_or_else(|| self.workspace());
        if workspace != self.workspace() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "this runtime has no admitted MCP audience for that workspace",
            ));
        }
        Ok(workspace.to_owned())
    }

    /// The profile's fixed actor, as which MCP callers act.
    fn actor(&self) -> &str;

    fn action_provider(&self) -> io::Result<Self::ActionProvider>;

    /// Construct one transport run for this exact configured audience. Hosts
    /// with original certified ownership override this; client labels never enter.
    fn mcp_session_factory(
        &self,
        workspace: &str,
    ) -> io::Result<tilo_mcp::SessionFactory<Self::ActionProvider>> {
        let workspace = self.mcp_workspace(Some(workspace))?;
        let host = self.action_provider()?;
        let actor = self.actor().to_owned();
        Ok(std::sync::Arc::new(move || {
            Ok(tilo_mcp::McpServer::new(
                host.clone(),
                workspace.clone(),
                actor.clone(),
            ))
        }))
    }

    /// Spawn `command` as `app_id`, connected to this session's services.
    fn launch(&self, app_id: &str, command: Command) -> io::Result<Child>;

    /// Where MCP answers read the content apps stored; `None` for a host
    /// without a content service.
    fn media(&self) -> Option<std::sync::Arc<dyn tilo_mcp::MediaReader>> {
        None
    }

    /// The person's other devices, as this host asks them from its own endpoint: where a
    /// `--connect --device` client works. `None` for a host that reaches none.
    fn other_devices(&self) -> Option<other_devices::OtherDevices> {
        None
    }
}
