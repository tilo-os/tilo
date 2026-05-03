//! Spawn helper that wraps `std::process::Command::new("bwrap")` with a
//! [`SandboxProfile`]'s rendered argv.
//!
//! Phase 1 keeps this layer deliberately thin so higher-level launchers
//! (workspace activation, capability portal, etc.) can plug in without
//! reaching into `Command` plumbing themselves.

use std::process::{Child, Command};

use super::profile::SandboxProfile;

/// Spawn `command` inside the sandbox described by `profile`.
///
/// Takes the profile by value because [`SandboxProfile::into_argv`]
/// consumes `self`. The plan permits either ownership shape; owning
/// here means callers can build the profile inline at the call site
/// (`spawn_in_workspace(SandboxProfile::for_workspace_test(...), &cmd)`)
/// without an explicit `.clone()`.
///
/// The returned [`Child`] is not waited on — callers decide whether to
/// stream output, attach pipes, or just `wait()`.
pub fn spawn_in_workspace(
    profile: &SandboxProfile,
    command: &[String],
) -> std::io::Result<Child> {
    // Clone the profile so we can keep the borrow shape the plan asks
    // for (`&SandboxProfile`) while still feeding the consuming
    // `into_argv`. Profile is cheap-to-clone (a few `Vec`s and a small
    // `HashMap`), and cloning at the launch boundary avoids subtle
    // ownership puzzles when the caller wants to inspect the profile
    // after spawning (e.g. for logging).
    let argv = clone_profile(profile).into_argv(command);
    Command::new("bwrap").args(&argv).spawn()
}

/// Manual clone helper. We don't derive `Clone` on `SandboxProfile`
/// itself because that would silently invite cloning at use-sites where
/// the plan explicitly wants `into_argv`'s consume-by-value contract;
/// keeping the clone local to the spawn helper documents the trade-off.
fn clone_profile(p: &SandboxProfile) -> SandboxProfile {
    SandboxProfile {
        workspace_id: p.workspace_id,
        binds_ro: p.binds_ro.clone(),
        binds_rw: p.binds_rw.clone(),
        env: p.env.clone(),
        uid: p.uid,
        gid: p.gid,
        seccomp: p.seccomp.clone(),
        wayland_socket: p.wayland_socket.clone(),
        dbus_socket: p.dbus_socket.clone(),
        net_isolated: p.net_isolated,
    }
}
