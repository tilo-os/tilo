//! Launching an application qOS did not package.
//!
//! Every other module in this crate exists to build a sandbox. This one
//! exists because there are applications for which that is the wrong
//! thing to do, and doing it anyway produces an app that does not start
//! rather than an app that is safer.
//!
//! ## Why a host application is not sandboxed
//!
//! A `.desktop` entry is a text file naming a command line, not a
//! package manifest: nothing in it says which paths, devices, services
//! or network hosts the program needs. qOS's sandbox is *derived* from
//! those declarations — [`crate::sandbox`] turns a manifest into bind
//! mounts, [`crate::seccomp`] into a syscall filter, [`crate::landlock`]
//! into an FS ruleset. With no manifest there's nothing to derive, and a
//! sandbox invented here would be a guess presented as policy.
//!
//! For a Flatpak specifically it's not even a guess that could work:
//! `flatpak run` builds its own sandbox, needing `unshare(2)`,
//! `mount(2)` and `pivot_root(2)`, all of which [`crate::seccomp`]'s
//! baseline denies, plus `/usr/bin/flatpak`, its installation tree and
//! system D-Bus (none of which qOS's sandbox binds), plus a network
//! namespace, which `--unshare-all` removes. Nesting the two doesn't
//! double the confinement — it produces `bwrap: execvp flatpak: No such
//! file or directory`, or a seccomp kill.
//!
//! ## What the user is therefore told
//!
//! A host application runs with the user's own identity, files and
//! network, exactly as under any other desktop: no `QOS_WORKSPACE_ID`
//! (no collaboration room, no per-room partition), no capability file
//! (workspace grants describe what a *sandboxed* app may reach, and
//! passing them here would assert a limit that doesn't exist), and no
//! `QOS_APP_DIR` (it writes to the user's real `$HOME`).
//!
//! Those absences are why the desktop labels host applications
//! separately and why this module prints a one-line notice naming the
//! app it's about to run unconfined — the risk managed is not escape
//! (there was never a sandbox to escape), it's a user who can't tell
//! the two kinds of application apart.
//!
//! ## What *is* controlled
//!
//! The environment. See [`desktop_entry::launch`]: the child is spawned
//! from an explicit allowlist plus the display handoff, so the
//! compositor's own `QOS_*` variables don't leak into a process not
//! subject to them.

use std::process::ExitStatus;

use desktop_entry::launch::{HostLaunch, HostLaunchError, LaunchEnv, plan_by_id};

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    Plan(#[from] HostLaunchError),
    #[error("spawning {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

impl HostError {
    pub fn to_exit_code(&self) -> i32 {
        2
    }
}

/// Resolve `desktop_id` on this system and run it, unconfined. Blocks
/// until the application exits, relaying its exit status — the same
/// shape as [`crate::run::run_with`], so the compositor's launch worker
/// treats both kinds of launch identically.
pub fn run_host(desktop_id: &str, env: &LaunchEnv) -> Result<ExitStatus, HostError> {
    let launch = plan_host(desktop_id, env)?;
    spawn(&launch)
}

/// Resolve `desktop_id` into the exact command line and environment a
/// launch would use, without starting anything — so "what will this
/// actually run, and with what?" is answerable before it runs, the
/// question that matters most for a launch with no sandbox around it.
pub fn plan_host(desktop_id: &str, env: &LaunchEnv) -> Result<HostLaunch, HostError> {
    Ok(plan_by_id(desktop_id, env)?)
}

/// Run an already-planned launch. Split out so a caller that built the
/// plan itself doesn't have to resolve the entry twice.
pub fn spawn(launch: &HostLaunch) -> Result<ExitStatus, HostError> {
    // Deliberately on stderr and unconditional: a launch that skips the
    // sandbox says so where the compositor's log, journal and a
    // developer's terminal will all see it.
    eprintln!(
        "qos-launcher: running host application {} ({}) UNCONFINED — \
         no qOS sandbox, no workspace room, no per-room state. {} {}",
        launch.desktop_id,
        launch.source.label(),
        launch.program.display(),
        launch.args.join(" "),
    );
    debug_assert!(
        !launch.is_confined(),
        "a host launch must never claim confinement"
    );

    let mut cmd = launch.to_command();
    cmd.stdin(std::process::Stdio::null());
    let mut child = cmd.spawn().map_err(|source| HostError::Spawn {
        program: launch.program.display().to_string(),
        source,
    })?;
    child.wait().map_err(|source| HostError::Spawn {
        program: launch.program.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{LaunchMode, RunError, RunOptions};

    /// A perfectly valid package manifest — the refusal being tested has
    /// to come from the launch *mode*, not from anything wrong with the
    /// package.
    fn example_manifest() -> crate::manifest::MinimalManifest {
        toml::from_str(
            "[package]\nid = \"dev.qos.example\"\nversion = \"1.0.0\"\n\
             [entrypoint]\nbin = \"bin/example\"\n",
        )
        .expect("the fixture manifest parses")
    }

    /// The guard that keeps a host application out of the sandbox
    /// builder. Drives the real `run_with`, not a copy of its check.
    #[test]
    fn run_with_refuses_a_host_launch_before_it_builds_anything() {
        let manifest = example_manifest();
        let opts = RunOptions {
            mode: LaunchMode::Host {
                desktop_id: "com.spotify.Client".into(),
            },
            ..Default::default()
        };
        let err = crate::run::run_with(&manifest, std::path::Path::new("/nonexistent"), &opts)
            .expect_err("a host application is not a package");
        match err {
            RunError::HostAppNotAPackage { desktop_id } => {
                assert_eq!(desktop_id, "com.spotify.Client");
            }
            other => panic!("expected HostAppNotAPackage, got {other:?}"),
        }
    }

    #[test]
    fn run_with_captured_refuses_it_too() {
        let manifest = example_manifest();
        let opts = RunOptions {
            mode: LaunchMode::Host {
                desktop_id: "org.gnome.TextEditor".into(),
            },
            ..Default::default()
        };
        let err =
            crate::run::run_with_captured(&manifest, std::path::Path::new("/nonexistent"), &opts)
                .expect_err("the second door into the sandbox builder is closed too");
        assert!(matches!(err, RunError::HostAppNotAPackage { .. }));
    }

    /// A launch of something that is not installed fails with a name,
    /// not a silent no-op.
    #[test]
    fn an_unknown_desktop_id_is_reported() {
        let err = run_host(
            "dev.qos.definitely-not-installed-anywhere",
            &LaunchEnv::default(),
        )
        .expect_err("nothing has that id");
        assert!(
            err.to_string()
                .contains("dev.qos.definitely-not-installed-anywhere"),
            "the error must name what could not be found: {err}"
        );
    }
}
