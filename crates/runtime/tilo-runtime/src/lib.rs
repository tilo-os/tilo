//! The per-user shared runtime: one private profile whose daemon hosts the
//! identity, collaboration and content services independently launched apps
//! attach to, and an MCP projection of its action host.
//!
//! This crate owns the profile, the native endpoint and app attachment, and serves `tilo_mcp`'s
//! projection; it reaches the services only through [`RuntimeServices`].
//! [`device::services::Daemon`] hosts them without the Tilo desktop: this computer's one daemon,
//! the `tilo-runtime` executable, which serves the person's login as its device too
//! ([`device`]).

use std::{
    io,
    path::Path,
    process::{Child, Command},
};

mod apps;
pub mod cli;
mod daemon;
pub mod device;
mod i18n;
pub mod key_sessions;
pub mod offload;
mod os_search;
pub mod other_devices;
pub mod personal_devices;
pub mod profile;
pub mod unix;

// Workspace admission decodes peer bytes in automerge worker processes started
// from this test binary; this serves them.
#[cfg(test)]
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

    /// The person's other devices, as this host reaches them; `None` for a host that reaches
    /// none, which offers an agent no device to work on.
    fn other_devices(&self) -> Option<crate::other_devices::OtherDevices> {
        None
    }

    /// Construct one transport run for this exact configured audience: the person's agent,
    /// under a key of its own. Client labels never enter.
    fn mcp_session_factory(
        &self,
        workspace: &str,
    ) -> io::Result<tilo_mcp::SessionFactory<Self::ActionProvider>>;

    /// Spawn `command` as `app_id`, connected to this session's services.
    fn launch(&self, app_id: &str, command: Command) -> io::Result<Child>;

    /// Where MCP answers read the content apps stored; `None` for a host
    /// without a content service.
    fn media(&self) -> Option<std::sync::Arc<dyn tilo_mcp::MediaReader>> {
        None
    }
}
