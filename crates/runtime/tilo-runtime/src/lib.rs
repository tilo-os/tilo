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
#[cfg(target_os = "macos")]
pub mod device_grants;
#[cfg(target_os = "macos")]
mod i18n;
pub mod mcp;
pub mod mcp_http;
pub mod mcp_process;
mod os_search;
pub mod profile;
pub mod standalone;
pub mod unix;
pub mod viewer;
pub mod watch;

// Room admission decodes peer bytes in automerge worker processes started
// from this test binary; this serves them.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();

/// Socket names inside a profile, shared by the daemon that binds them and
/// the launcher that attaches an app to them.
pub const COLLAB_SOCKET: &str = "collab.sock";
pub const IDENTITY_SOCKET: &str = "identity.sock";
pub const BLOB_SOCKET: &str = "blobs.sock";
/// The runtime's own actions (the device grants), for `tilo-control
/// --socket`.
pub const CONSOLE_SOCKET: &str = "console.sock";
/// The profile's workspace files, served read only (`tilo-file-provider`).
pub const FILES_SOCKET: &str = "files.sock";

/// Joins a room another host governs from an invitation, answering the
/// joined room's id.
pub type Joiner = std::sync::Arc<dyn Fn(&str) -> io::Result<String> + Send + Sync>;

/// Reads content an app of this profile stored, through that app's own
/// scope, so an answer can carry what an action produced (the picture a
/// `look` rendered) instead of only naming it.
pub trait MediaReader: Send + Sync {
    /// The bytes of `hash` as `app_id` stored them; `Ok(None)` when that app
    /// may not read them or they are not here.
    /// # Errors
    /// Returns connection errors.
    fn read(&self, app_id: &str, hash: &str) -> io::Result<Option<Vec<u8>>>;
}

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

    /// Start an isolated admission session with the already enrolled device
    /// owner's identity. This must only verify that identity: no provisioning,
    /// certificate renewal or identity/fleet persistence is permitted.
    /// # Errors
    /// Refuses unsupported hosts, foreign accounts, absent or invalid identity.
    fn start_admission_reporting(
        root: &Path,
        owner_uid: u32,
        stage: &mut dyn FnMut(&str),
    ) -> io::Result<Self> {
        let _ = (root, owner_uid, stage);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this host has no native admission profile",
        ))
    }

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

    /// Select the fixed audience of a host-configured MCP endpoint. This
    /// never enrolls a room or changes grants. Hosts supporting shared rooms
    /// must check their current certified admission and the profile actor.
    ///
    /// # Errors
    /// Refuses any audience this host cannot currently authenticate.
    fn mcp_room(&self, requested: Option<&str>) -> io::Result<String> {
        let room = requested.unwrap_or_else(|| self.room());
        if room != self.room() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "this runtime has no admitted MCP audience for that room",
            ));
        }
        Ok(room.to_owned())
    }

    /// The profile's fixed actor, as which MCP callers act.
    fn actor(&self) -> &str;

    /// # Errors
    /// Returns an error when the action broker is unavailable.
    fn action_host(&self) -> io::Result<Self::ActionHost>;

    /// Construct one transport run for this exact configured audience. Hosts
    /// with original certified custody override this; client labels never enter.
    ///
    /// # Errors
    /// Rejects unavailable admission, an invalid audience or a missing action host.
    fn mcp_session_factory(&self, room: &str) -> io::Result<mcp::SessionFactory<Self::ActionHost>> {
        let room = self.mcp_room(Some(room))?;
        let host = self.action_host()?;
        let actor = self.actor().to_owned();
        Ok(std::sync::Arc::new(move || {
            Ok(mcp::McpServer::new(
                host.clone(),
                room.clone(),
                actor.clone(),
            ))
        }))
    }

    /// How this host joins rooms other hosts govern; `None` for a host
    /// that joins through its own surfaces.
    fn joiner(&self) -> Option<Joiner> {
        None
    }

    /// Spawn `command` as `app_id`, connected to this session's services.
    /// # Errors
    /// Returns handshake, descriptor or process-spawn errors.
    fn launch(&self, app_id: &str, command: Command) -> io::Result<Child>;

    /// Where MCP answers read the content apps stored; `None` for a host
    /// without a content service.
    fn media(&self) -> Option<std::sync::Arc<dyn MediaReader>> {
        None
    }
}
