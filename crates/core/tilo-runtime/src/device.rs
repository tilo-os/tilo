//! This computer as one of the person's devices, without the Tilo desktop: the one daemon
//! (`tilo-runtime`) that the CLI, a server and tilo.sh all run.
//!
//! It keeps the person's workspaces in the device's own workspace service
//! ([`workspaces::ServiceWorkspaces`]), each a copy synced with the device that serves it, the
//! login's workspace among them, and serves on one endpoint under the device's key:
//!
//! - the apps of this computer, which attach to the collaboration broker, identity service and
//!   content store it runs over those workspaces ([`services::Daemon`]);
//! - every agent's `tilo mcp`, each a connection to the one socket it listens on ([`serve`]),
//!   answered by the daemon's own workspace as the apps are, each a run of the one agent the
//!   login was made for; the device's session with its host shows that agent present
//!   ([`presence`]);
//! - Tilo's API as this computer's key, which `tilo do`, `ls` and `read` reach;
//! - the person's workspaces' folders below their home ([`peer`]), listed and entered through
//!   their server's account ([`account`]).
//!
//! Every connection is one agent's work of its own, shown by the session the agent's harness
//! names (`session_name`: "Office Hours"), else by the agent's product ("Claude Code"), which
//! History groups its actions under.

pub(crate) mod account;
/// The workspace's connected accounts, read by the apps attached here as `accounts`.
mod accounts;
#[cfg(target_os = "linux")]
mod agent_tools;
#[cfg(test)]
mod app_listing_tests;
/// An app's place in its workspace's live presence, on the connection its launcher handed it.
mod app_presence;
/// The project checks this device approved: its work, kept as its jobs. A check's commands are
/// ush's own, which runs only on Linux.
#[cfg(target_os = "linux")]
pub(crate) mod approved_checks;
mod asked;
pub(crate) mod audio_output;
mod boot_unlock;
#[cfg(test)]
mod boot_unlock_tests;
mod call_media;
mod carry;
// What the daemon's program mounts beside the core: services, and the machine's own hooks.
pub mod capabilities;
/// What the person copies on one device continuing on their others: the daemon carries it.
pub mod clipboard;
mod commits;
mod compiled;
mod content;
mod dev_builds;
mod dispatch;
/// The workspace's environments, reached by any app as an ordinary action call and answered by
/// the shell runtime this device runs.
#[cfg(target_os = "linux")]
mod environment_directory;
#[cfg(all(test, target_os = "linux"))]
mod environment_directory_tests;
/// The workspace's environments as a view reads and follows them, kept current from the records
/// the workspace holds.
#[cfg(target_os = "linux")]
mod environment_readings;
#[cfg(target_os = "linux")]
pub mod environments;
/// This device's shell runtime (`ush`): what it asks the daemon, and the one client every
/// provider of the daemon asks it through.
pub(crate) mod shell_runtime;
// The device's content, and on Tilo OS the steps that change it; elsewhere a device offers no steps.
pub(crate) mod confined;
pub mod control;
mod files_tree;
#[cfg(test)]
mod files_tree_tests;
pub(crate) mod first_workspace;
#[cfg(test)]
mod flow_laptop_phone_server;
/// A workspace of the person's made on the device that founds their workspaces, asked from another.
pub(crate) mod founding_create;
#[cfg(test)]
mod founding_tests;
pub(crate) mod home;
mod host_runs;
mod host_shells;
mod hosts;
pub(crate) mod jobs;
pub mod keep;
pub(crate) mod key_use;
pub(crate) mod layout;
mod live;
mod live_call;
mod live_relay;
mod live_speaking;
pub(crate) mod local;
mod local_files;
/// Where this device is: the platform's location service, answered as the device's own action
/// (`devices/<device>/location#locate`), never reached from an app's process.
pub mod location;
mod mcp_server;
pub(crate) mod migrate;
/// The workspace's model providers, read by the apps attached here as `models`.
mod models;
pub mod nearby;
mod offload_place;
pub mod online;
mod own_device;
pub mod package_runs;
pub mod packages;
mod page_media;
#[cfg(test)]
mod page_ticket_tests;
pub mod peer;
pub(crate) mod presence;
pub mod processes;
/// The resources the daemon publishes to the apps attached to it: one table, a provider each.
pub mod published;
mod reading;
#[cfg(test)]
mod record_actions_tests;
pub(crate) mod records;
#[cfg(test)]
mod root_holder_login_tests;
mod search;
mod secret_clipboard;
pub mod serve;
pub mod server_settings;
pub mod services;
mod shell;
#[cfg(target_os = "linux")]
pub(crate) mod ush_line;

/// How the person's workspaces stand, as the daemon publishes them to its person's shells.
pub use shell::Standing;
pub mod issuing;
pub mod shortcuts;
pub(crate) mod signing;
#[cfg(not(windows))]
mod styles;
pub mod take_work;
#[cfg(target_os = "linux")]
mod task_programs;
#[cfg(target_os = "linux")]
pub mod tasks;
#[cfg(target_os = "linux")]
pub(crate) mod terminal;
pub(crate) mod ticket;
#[cfg(test)]
mod ticket_tests;
pub(crate) mod unclaimed;
#[cfg(test)]
mod unclaimed_tests;
/// Another of the person's devices at early boot asking for its disk-unlock half.
pub(crate) mod unlock;
// A vault item released to the app process whose connection asks for it.
pub(crate) mod store_lane;
pub(crate) mod vault_release;
pub(crate) mod version_notice;
#[cfg(target_os = "linux")]
pub(crate) mod vm_environment;
pub mod wake;
pub mod wifi;
mod window_follow;
pub mod workspace_sync;
pub use window_follow::provide_window_steps;
mod window_notes;
pub mod workspaces;

use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};
use tilo_device_identity::login::Device;
use tilo_lock::MutexRecover as _;

/// How serving the device ended ([`hold`]).
pub(crate) enum Ended {
    /// The daemon was told to stop, or another took the device over.
    Stopped,
    /// This computer logged in, or its person's own workspace names a server it did not enter
    /// through: its person's part starts again as that device, entering through that server.
    LoggedIn(Box<Device>),
}

/// Serve this computer's device as `daemon` is: every agent's `tilo mcp`, the person's workspace
/// commands and their workspaces' folders, until the daemon is told to stop, another takes the
/// device over, or this computer logs in with a ticket. A computer whose person's server is not
/// known yet serves them too, each saying what it waits for.
///
/// # Errors
/// The device's sockets cannot be listened at.
pub(crate) fn hold(daemon: &Arc<services::Daemon>, stop: &serve::Stop) -> Result<Ended> {
    let home = tilo_paths::home().ok_or_else(|| anyhow::anyhow!("HOME is missing"))?;
    serve::hold(daemon, home, stop)
}

/// The presence of the device this process serves, while it serves one: what its native
/// endpoint answers [`tilo_api::daemon::PEER_STATUS`] about.
static HELD: std::sync::Mutex<Option<Arc<presence::Presence>>> = std::sync::Mutex::new(None);

/// Say which presence this process serves the device with, or that it serves none now.
pub(crate) fn serving(presence: Option<Arc<presence::Presence>>) {
    *HELD.lock_recover() = presence;
}

/// The device this computer is as it enters its person's workspaces, while its person's part
/// runs and knows their server: what `account#who` answers as its `home`.
pub(crate) fn home() -> Option<Device> {
    let held = HELD.lock_recover().clone();
    held?.home()
}

/// This process's answer to [`tilo_api::daemon::PEER_STATUS`]: the served device's, or, before
/// its person's services run, that it has none yet.
pub(crate) async fn status() -> Value {
    let held = HELD.lock_recover().clone();
    match held {
        Some(presence) => peer_status(&presence).await,
        None => json!({
            "pid": std::process::id(),
            "device": null,
            "person": null,
            "host": null,
            "session": "starting",
            "not_in": "this device's person's services are not running yet",
            "mounted": [],
            "workspaces": [],
            "workspaces_complete": false,
            "failed": [],
        }),
    }
}

/// The peer's answer to [`tilo_api::daemon::PEER_STATUS`].
pub(crate) async fn peer_status(presence: &presence::Presence) -> Value {
    let device = presence.home();
    let device = device.as_ref();
    let session = presence.current();
    let phase = session.as_ref().map_or_else(
        || "entering".to_owned(),
        |session| format!("{:?}", session.device.session().status().phase).to_lowercase(),
    );
    drop(session);
    let (mounted, failed, workspaces, complete) = match serve::peer() {
        Some(peer) => {
            let (workspaces, complete) = peer.workspaces().await;
            (peer.mounted().await, peer.failures(), workspaces, complete)
        }
        None => (Vec::new(), Vec::new(), Vec::new(), false),
    };
    let not_in = serve::NOT_IN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    json!({
        "pid": std::process::id(),
        "device": device.map(Device::label),
        "person": device.map(Device::user),
        "host": device.map(|device| &device.host_origin),
        "session": phase,
        "not_in": not_in,
        "mounted": mounted,
        "workspaces": workspaces,
        "workspaces_complete": complete,
        "failed": failed.into_iter().map(|(workspace, why)| json!({"workspace": workspace, "why": why})).collect::<Vec<_>>(),
    })
}
