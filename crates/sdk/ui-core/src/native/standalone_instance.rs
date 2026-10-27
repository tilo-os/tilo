//! Another window of a native app outside the desktop's tab model: another
//! process of the same executable.

/// Start another process of this executable showing `target`.
pub(crate) fn open(target: Option<&str>) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = instance_command(&executable, target, std::env::vars_os())?;
    let mut child = command
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    crate::background::Background::current()
        .spawn("standalone-instance-reaper", move || {
            let _ = child.wait();
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Whether `name` hands this process one of its runtime connections: an
/// inherited descriptor number, or the record of which process received them.
fn names_runtime_connection(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    ((name.starts_with("TILO_SVC_") || name.starts_with("TILO_RUNTIME_")) && name.ends_with("_FD"))
        || name == "TILO_RUNTIME_CONNECTION_OWNER_PID"
        || name == "TILO_RUNTIME_RESUME_LIVE"
}

/// The command for another process of `executable` showing `target`.
///
/// This process's runtime connections are descriptors it holds and closes
/// across exec, so their names in `environment` never reach the new process:
/// a child inheriting them would find the numbers naming nothing (or
/// something else) and refuse to start. Without them it attaches on its own.
fn instance_command(
    executable: &std::path::Path,
    target: Option<&str>,
    environment: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Result<std::process::Command, String> {
    let mut command = std::process::Command::new(executable);
    if let Some(target) = target {
        // One opaque positional, never something an argument parser
        // could read as a flag.
        if target.starts_with('-') {
            return Err("refusing an argument-shaped target".into());
        }
        command.arg(target);
    }
    for (name, _) in environment {
        if names_runtime_connection(&name) {
            command.env_remove(name);
        }
    }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::instance_command;
    use std::ffi::{OsStr, OsString};
    use std::path::Path;

    #[test]
    fn another_window_starts_without_this_processs_runtime_connections() {
        let environment = [
            ("TILO_SVC_COLLAB_FD", "17"),
            ("TILO_SVC_IDENTITY_FD", "31"),
            ("TILO_SVC_MODEL-GATEWAY_FD", "19"),
            ("TILO_RUNTIME_LAUNCH_GUARD_FD", "23"),
            ("TILO_RUNTIME_CONNECTION_OWNER_PID", "42"),
            ("TILO_RUNTIME_RESUME_LIVE", "so.tilo.file-explorer"),
            ("TILO_RUNTIME_PROFILE", "dev"),
            ("TILO_PACKAGE_ID", "so.tilo.file-explorer"),
            ("WAYLAND_DISPLAY", "wayland-1"),
        ]
        .map(|(name, value)| (OsString::from(name), OsString::from(value)));
        let command = instance_command(
            Path::new("/app/bin/files"),
            Some("file:///home/a"),
            environment,
        )
        .unwrap();
        let mut removed: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        removed.sort();
        assert_eq!(
            removed,
            [
                "TILO_RUNTIME_CONNECTION_OWNER_PID",
                "TILO_RUNTIME_LAUNCH_GUARD_FD",
                "TILO_RUNTIME_RESUME_LIVE",
                "TILO_SVC_COLLAB_FD",
                "TILO_SVC_IDENTITY_FD",
                "TILO_SVC_MODEL-GATEWAY_FD",
            ]
        );
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("file:///home/a")]
        );
        assert!(
            instance_command(Path::new("/app/bin/files"), Some("--x"), []).is_err(),
            "an argument-shaped target is refused"
        );
    }
}
