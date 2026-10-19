//! Runs a command the way the launcher runs the Browser's Chromium engine:
//! with the environment a Browser launch hands on, under the two syscall
//! filters its sandbox stacks (the `clone3` compatibility filter, then the
//! baseline profile the image grants the Browser's manifest), the order
//! `bwrap --add-seccomp-fd` installs them in. The engine builds its own
//! renderer sandbox inside, as it does on a device.
//!
//! The `chromium-host` check loads real pages through it, so a launch setting
//! that kills the engine (an environment variable its sandbox cannot live
//! with, a syscall the profile refuses) fails there instead of on a device.
//! Grants nothing: it only narrows the process it replaces.
#[cfg(target_os = "linux")]
use std::error::Error;

#[cfg(target_os = "linux")]
const BROWSER_MANIFEST: &str = include_str!("../../../../apps/browser/tilo-app.toml");

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn Error>> {
    use std::os::unix::process::CommandExt;

    let mut arguments = std::env::args_os().skip(1);
    let program = arguments
        .next()
        .ok_or("usage: web_engine_sandbox PROGRAM [ARGS…]")?;
    let manifest: toml::Value = toml::from_str(BROWSER_MANIFEST)?;
    let package_id = manifest
        .get("package")
        .and_then(|package| package.get("id"))
        .and_then(toml::Value::as_str)
        .ok_or("the Browser manifest names no package id")?;
    let requested = manifest
        .get("sandbox")
        .and_then(|sandbox| sandbox.get("seccomp"))
        .and_then(toml::Value::as_str)
        .ok_or("the Browser manifest names no seccomp profile")?;
    let profile = launcher::seccomp::resolve_profile(requested, package_id).applied;

    // The variables a Browser launch sets, from the launcher's own builders.
    // The caller's environment stays underneath (the check's HOME and
    // TMPDIR stand in for the sandbox's own).
    let mut launch = Vec::new();
    launcher::sandbox::push_clean_environment(&mut launch);
    launcher::sandbox::push_runtime_hardening(
        &mut launch,
        &launcher::hardening::for_package(package_id),
    );
    let mut command = std::process::Command::new(program);
    command.args(arguments);
    for window in launch.windows(3) {
        if let [flag, key, value] = window
            && flag == "--setenv"
            && key != "PATH"
        {
            command.env(key, value);
        }
    }
    // As `browser_continuity_core::engine_host::command` starts the engine.
    command
        .env_remove("LD_PRELOAD")
        .env_remove("LD_LIBRARY_PATH");

    for filter in [
        launcher::seccomp::compile_clone3_compat()?,
        launcher::seccomp::compile_app_baseline(
            profile,
            launcher::seccomp::SyscallGrants::default(),
        )?,
    ] {
        sandbox_core::seccomp::install_bytes(&filter)?;
    }
    eprintln!(
        "web_engine_sandbox: {} under {}",
        package_id,
        profile.as_str()
    );
    Err(command.exec().into())
}

#[cfg(not(target_os = "linux"))]
fn main() {}
