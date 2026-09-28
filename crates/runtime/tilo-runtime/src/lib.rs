//! The per-user shared runtime: one private profile whose daemon hosts the
//! identity, collaboration and content services independently launched apps
//! attach to, and an MCP projection of its action host.
//!
//! The services are the host's: a desktop session and a standalone profile
//! serve the same brokers. This crate owns the profile, the native endpoint,
//! app attachment and the MCP protocol; it reaches the services only through
//! [`RuntimeServices`]. [`standalone::Standalone`] hosts them without the
//! Tilo desktop, for the `tilo-runtime` executable.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::{
    io,
    path::Path,
    process::{Child, Command},
};

mod apps;
pub mod cli;
pub mod computer;
mod daemon;
pub mod mcp;
pub mod mcp_http;
pub mod mcp_process;
pub mod profile;
pub mod standalone;
pub mod unix;

// Room admission decodes peer bytes in automerge worker processes started
// from this test binary; this serves them.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();

/// Socket names inside a profile, shared by the daemon that binds them and
/// the launcher that attaches an app to them.
pub const COLLAB_SOCKET: &str = "collab.sock";
pub const IDENTITY_SOCKET: &str = "identity.sock";
pub const BLOB_SOCKET: &str = "blobs.sock";

/// One running service session over a profile: the host's identity,
/// collaboration and content services.
pub trait RuntimeServices: Sized {
    /// The action host MCP callers browse and invoke through.
    type ActionHost: tilo_actions::remote::ActionHost + Clone + Send + Sync + 'static;

    /// Start the services on `root`, naming each step to `stage` before taking
    /// it, so whoever waits can tell a slow start from a stuck one. Holds the
    /// profile exclusively until dropped.
    /// # Errors
    /// Rejects a profile in use, invalid persisted state or failed provisioning.
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

    /// The workspace room every app of this profile joins.
    fn room(&self) -> &str;

    /// The profile's fixed actor, as which MCP callers act.
    fn actor(&self) -> &str;

    /// # Errors
    /// Returns an error when the action broker is unavailable.
    fn action_host(&self) -> io::Result<Self::ActionHost>;

    /// Spawn `command` as `app_id`, connected to this session's services.
    /// # Errors
    /// Returns handshake, descriptor or process-spawn errors.
    fn launch(&self, app_id: &str, command: Command) -> io::Result<Child>;
}
