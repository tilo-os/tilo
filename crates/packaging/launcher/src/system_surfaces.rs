//! Whether a sandboxed app acts on the machine it runs on.
//!
//! The compositor settles, where it chooses its backend, whether it owns the
//! machine's system surfaces (network, radios, audio, displays, clock,
//! accounts, printers, power, software) and publishes the answer in
//! [`SYSTEM_SURFACES_ENV`], which the launcher inherits. A sandbox starts
//! from a cleared environment, so without this module no sandboxed app would
//! ever learn that answer and every system app would treat the machine as
//! managed by someone else.
//!
//! The launcher passes the answer into an app only when both of these hold:
//!
//! - the manifest declares `[sandbox] manages-machine = true`, and
//! - the image lists the package in [`POLICY_PATH`].
//!
//! The manifest only asks: a `.tapp` carries its own manifest, so a package
//! declaring it manages the machine is not authority to be told it may. This
//! is the same rule the seccomp and hardening policies follow. Every other
//! app is started without the variable, which reads as "hosted".
//!
//! The answer is an accident guard for system apps, not a permission: what
//! the app can actually change is still decided by polkit, logind, file
//! permissions and the sandbox's binds.

use std::path::PathBuf;

use crate::manifest::MinimalManifest;

/// Carries the compositor's answer. Mirrors `os_actions::owner::ENV`,
/// duplicated since the launcher has no dependency on `os-actions`.
pub const SYSTEM_SURFACES_ENV: &str = "TILO_SYSTEM_SURFACES";

/// [`SYSTEM_SURFACES_ENV`]'s value when the session owns the machine; any
/// other value, or none, means hosted.
pub const OWNED: &str = "owned";

/// [`SYSTEM_SURFACES_ENV`]'s value for a system app on a machine the session
/// does not own.
pub const HOSTED: &str = "hosted";

/// Image-owned list of packages that may be told they manage the machine.
/// Same format as [`crate::seccomp::JIT_POLICY_PATH`].
pub const POLICY_PATH: &str = "/usr/lib/tilo/manages-machine.conf";

/// Overrides [`POLICY_PATH`], so a test can point at a fixture. Honoured only
/// by developer builds (see [`crate::dev_env`]).
pub const POLICY_PATH_ENV: &str = "TILO_MANAGES_MACHINE_FILE";

/// Fallback when [`POLICY_PATH`] is absent: the system apps in this tree that
/// change the machine itself. The installer is not here: it runs under its
/// own kiosk compositor, outside the launcher, and is told directly.
pub const BUILTIN_MANAGES_MACHINE: &[&str] = &["so.tilo.settings", "so.tilo.vpn"];

/// Whether the image lets `package_id` be told it manages the machine.
pub fn entitled(package_id: &str) -> bool {
    let policy_path = crate::dev_env::dev_override(POLICY_PATH_ENV)
        .map_or_else(|| PathBuf::from(POLICY_PATH), PathBuf::from);
    match std::fs::read_to_string(&policy_path) {
        Ok(list) => crate::hardening::policy_matches(&list, package_id),
        Err(_) => BUILTIN_MANAGES_MACHINE.contains(&package_id),
    }
}

/// Whether the session that started this launcher owns the machine.
pub fn session_owns_machine() -> bool {
    std::env::var(SYSTEM_SURFACES_ENV).is_ok_and(|value| value.trim() == OWNED)
}

/// The value a launch of `manifest` gets, or `None` for an app that is not a
/// system app and is started without the variable.
pub fn value_for(manifest: &MinimalManifest) -> Option<&'static str> {
    if !manifest.sandbox.manages_machine {
        return None;
    }
    if !entitled(&manifest.package_id) {
        eprintln!(
            "tilo-launcher: {} declares that it manages the machine, but the image does not \
             list it in {POLICY_PATH}; starting it as hosted",
            manifest.package_id
        );
        return Some(HOSTED);
    }
    Some(if session_owns_machine() {
        OWNED
    } else {
        HOSTED
    })
}

/// Splice the answer into a sandbox argv built by
/// [`crate::sandbox::build_bwrap_argv`], after its `--clearenv`.
pub fn push_system_surfaces(argv: &mut Vec<String>, manifest: &MinimalManifest) {
    if let Some(value) = value_for(manifest) {
        argv.extend([
            "--setenv".to_string(),
            SYSTEM_SURFACES_ENV.to_string(),
            value.to_string(),
        ]);
    }
}
