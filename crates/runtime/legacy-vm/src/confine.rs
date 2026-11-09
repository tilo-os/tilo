//! `tilo-legacy confine-fs`: Landlock a helper to the paths it serves, then
//! exec it. Used for every `virtiofsd` (see [`crate::virtiofs`]) and every
//! hosted VM's share server ([`crate::hosted::files`]).
//!
//! Landlock decides what a helper may open, but not which Unix sockets it
//! may connect to. A hosted VM's share server is reached by a stranger's
//! VM, so it is also isolated (`--isolate`): it gets user, mount, network
//! and IPC namespaces of its own, in which the directories named with
//! `--hide` (the jails with every VM's API socket, the service's own
//! socket) are empty read-only mounts and there is no network at all.
//! Landlock then forbids it to change its mounts, so it cannot uncover
//! them.

use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::Command;

use crate::virtiofs::Confinement;
use sandbox_core::landlock::{
    LandlockOutcome, NetRestriction, RulesetSpec, TARGET_ABI, access_plan, outcome_from_status,
    restrict_self,
};

/// Restrict this process as `confinement` says and replace it with its
/// program. Returns only on failure: an unconfined helper is never started.
pub fn exec_confined(confinement: &Confinement) -> String {
    let Confinement {
        ro,
        rw,
        isolate,
        hide,
        program,
    } = confinement;
    if *isolate && let Err(error) = isolate_self(hide) {
        return format!("could not isolate the helper: {error}");
    }
    let spec = RulesetSpec::application(access_plan(ro, rw, TARGET_ABI), NetRestriction::DenyAll);
    let status = match restrict_self(&spec, |_, _| {}) {
        Ok(status) => status,
        Err(error) => return format!("could not confine the helper: {error}"),
    };
    let outcome = outcome_from_status(&status.ruleset);
    if outcome != LandlockOutcome::FullyEnforced {
        return format!(
            "refusing to start an unconfined helper ({})",
            outcome.describe()
        );
    }
    let Some((name, args)) = program.split_first() else {
        return "no program to run".to_string();
    };
    let error = Command::new(name).args(args).exec();
    format!("could not run {name}: {error}")
}

/// Namespaces of this process's own, mapping only its own ids: no network,
/// and `hide` covered by empty read-only mounts.
#[cfg(target_os = "linux")]
fn isolate_self(hide: &[PathBuf]) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;
    let (uid, gid) = (
        rustix::process::getuid().as_raw(),
        rustix::process::getgid().as_raw(),
    );
    let fail = |what: &str| {
        let error = std::io::Error::last_os_error();
        std::io::Error::new(error.kind(), format!("{what}: {error}"))
    };
    // SAFETY: plain system calls on this process.
    if unsafe {
        libc::unshare(
            libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWNET | libc::CLONE_NEWIPC,
        )
    } != 0
    {
        return Err(fail("unshare"));
    }
    std::fs::write("/proc/self/setgroups", "deny")?;
    std::fs::write("/proc/self/uid_map", format!("{uid} {uid} 1"))?;
    std::fs::write("/proc/self/gid_map", format!("{gid} {gid} 1"))?;
    // SAFETY: constant arguments.
    if unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(fail("making mounts private"));
    }
    for dir in hide.iter().filter(|dir| dir.is_dir()) {
        let target = CString::new(dir.as_os_str().as_bytes())?;
        // SAFETY: the strings live across the call.
        if unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                target.as_ptr(),
                c"tmpfs".as_ptr(),
                libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
                c"size=4k,mode=000".as_ptr().cast(),
            )
        } != 0
        {
            return Err(fail(&format!("hiding {}", dir.display())));
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn isolate_self(_: &[PathBuf]) -> std::io::Result<()> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}
