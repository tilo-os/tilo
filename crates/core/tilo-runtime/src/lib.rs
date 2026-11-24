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
#[cfg(unix)]
pub mod cli;
#[cfg(unix)]
pub mod daemon;
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
#[cfg(unix)]
mod seats;

// Workspace admission decodes peer bytes in automerge worker processes started
// from this test binary; this serves them.
#[cfg(all(test, unix))]
tilo_automerge_isolate::test_worker!();

/// Socket names in the directory the daemon keeps its sockets in, shared by the daemon
/// that binds them and the launcher that attaches an app to them.
pub const COLLAB_SOCKET: &str = "collab.sock";
pub const IDENTITY_SOCKET: &str = "identity.sock";
pub const BLOB_SOCKET: &str = "blobs.sock";

/// How long a process the daemon runs for an app goes on with no call before it ends: long
/// enough that an app called again finds it running, short enough that the apps of an afternoon
/// don't hold a small device's memory. It is the same for an app's process whichever language
/// the app is written in, and the call that comes after starts it again.
#[cfg(unix)]
pub const IDLE: std::time::Duration = std::time::Duration::from_secs(5 * 60);

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

    /// The workspace every app of this profile joins, once the person has one here.
    ///
    /// # Errors
    /// The person has no workspace here, and none can be founded for them.
    fn workspace(&self) -> io::Result<String>;

    /// Select the fixed audience of a host-configured MCP endpoint. This
    /// never enrolls a workspace or changes grants. Hosts supporting shared workspaces
    /// must check their current certified admission and the profile actor.
    ///
    /// # Errors
    /// The runtime has no admitted audience for `requested`.
    fn mcp_workspace(&self, requested: Option<&str>) -> io::Result<String>;

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
