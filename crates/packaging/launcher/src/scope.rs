//! One systemd user scope per launched app.
//!
//! Every launch — a sandboxed Tilo package or an unconfined host
//! application — runs in its own transient `app-tilo-<id>-<pid>.scope` of
//! the systemd user manager, in `app.slice` and part of the graphical
//! session target. That makes an app one unit for resource accounting
//! (its memory is charged to its own cgroup, never the compositor's), for
//! the OOM killer, and for logout or a compositor crash, which stop the
//! app and everything it forked instead of leaving it running with no
//! display.
//!
//! The launcher moves *itself* into the scope before it forks the app, so
//! the app and every process it starts are born inside it rather than
//! chased into it afterwards. A manager that is unreachable (a headless
//! test host, a session without a user manager) or refuses costs the
//! scope, never the launch: the caller logs the reason and runs the app
//! where the launcher already is.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The systemd user manager's D-Bus front end.
pub const BUSCTL: &str = "/usr/bin/busctl";

/// Stopping this target at logout stops every app's scope.
const SESSION_TARGET: &str = "tilo-graphical-session.target";

/// The slice every app scope lives in, per the desktop-environment
/// convention (<https://systemd.io/DESKTOP_ENVIRONMENTS>).
const APP_SLICE: &str = "app.slice";

/// The longest unit name systemd accepts.
const UNIT_NAME_MAX: usize = 255;

/// How long the manager gets to move this process once it accepted the
/// scope. It runs the start job right after answering, so a healthy manager
/// needs milliseconds.
pub const JOIN_TIMEOUT: Duration = Duration::from_secs(1);

/// `app-tilo-<app id>-<pid>.scope`, with the id escaped as a unit name must
/// be: `-` separates the name's fields, so it and anything else outside
/// `[A-Za-z0-9:_.]` becomes `\xNN`. `None` when the result is too long to be
/// a unit name.
#[must_use]
pub fn unit_name(app_id: &str, pid: u32) -> Option<String> {
    use std::fmt::Write as _;
    let mut escaped = String::with_capacity(app_id.len());
    for byte in app_id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'.') {
            escaped.push(char::from(byte));
        } else {
            let _ = write!(escaped, "\\x{byte:02x}");
        }
    }
    let unit = format!("app-tilo-{escaped}-{pid}.scope");
    (!escaped.is_empty() && unit.len() <= UNIT_NAME_MAX).then_some(unit)
}

/// Move this process into a new scope for `app_id` through the system's
/// `busctl`, waiting until the move is visible in `/proc/self/cgroup`.
/// Returns the unit name.
///
/// # Errors
///
/// A human-readable reason for the log when there is no `busctl`, the id
/// cannot name a unit, the manager refuses, or it accepts but does not move
/// this process in time.
pub fn join(app_id: &str, description: &str) -> Result<String, String> {
    let busctl = Path::new(BUSCTL);
    if !busctl.is_file() {
        return Err(format!("no {BUSCTL} to reach the systemd user manager"));
    }
    join_with(busctl, app_id, description, JOIN_TIMEOUT)
}

/// [`join`] with the manager's front end and the join wait supplied.
///
/// # Errors
///
/// As [`join`].
pub fn join_with(
    busctl: &Path,
    app_id: &str,
    description: &str,
    timeout: Duration,
) -> Result<String, String> {
    let pid = std::process::id();
    let unit = unit_name(app_id, pid).ok_or_else(|| "its id cannot name a unit".to_string())?;
    // `StartTransientUnit` answers once the start job is queued, before the
    // move, hence the wait on this process's own cgroup below.
    let output = Command::new(busctl)
        .args(request_args(&unit, pid, description))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("running {}: {error}", busctl.display()))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let deadline = Instant::now().checked_add(timeout);
    loop {
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
        if cgroup_is_unit(&cgroup, &unit) {
            return Ok(unit);
        }
        if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
            return Err(format!(
                "{unit} was accepted but not joined within {timeout:?}"
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// `busctl` arguments for `StartTransientUnit` of a scope holding `pid`.
#[must_use]
pub fn request_args(unit: &str, pid: u32, description: &str) -> Vec<String> {
    let mut properties: Vec<Vec<String>> = vec![
        vec!["PIDs".into(), "au".into(), "1".into(), pid.to_string()],
        vec!["Slice".into(), "s".into(), APP_SLICE.into()],
        vec![
            "CollectMode".into(),
            "s".into(),
            "inactive-or-failed".into(),
        ],
        vec![
            "PartOf".into(),
            "as".into(),
            "1".into(),
            SESSION_TARGET.into(),
        ],
    ];
    // The person just asked for this app: it starts in the foreground tier,
    // and the compositor demotes it when focus moves on (see `priority`).
    let tier = crate::priority::tier_properties(
        crate::priority::Tier::Foreground,
        crate::priority::mem_total_bytes(),
    );
    properties.extend(tier.iter().map(|p| crate::priority::dbus_property_args(p).to_vec()));
    let description: String = description.chars().filter(|c| !c.is_control()).collect();
    if !description.trim().is_empty() {
        properties.push(vec!["Description".into(), "s".into(), description]);
    }
    let mut args: Vec<String> = [
        "--user",
        "--timeout=2",
        "call",
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        "StartTransientUnit",
        "ssa(sv)a(sa(sv))",
    ]
    .map(String::from)
    .to_vec();
    args.push(unit.to_string());
    args.push("fail".into());
    args.push(properties.len().to_string());
    args.extend(properties.into_iter().flatten());
    args.push("0".into());
    args
}

/// Whether a `/proc/<pid>/cgroup` text places the process in `unit`.
#[must_use]
pub fn cgroup_is_unit(cgroup: &str, unit: &str) -> bool {
    cgroup
        .lines()
        .any(|line| line.rsplit('/').next() == Some(unit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_request_names_its_unit_slice_session_and_process() {
        let request = request_args("app-tilo-com.spotify.Client-42.scope", 42, "Spo\ntify");
        // The foreground tier rides between the session link and the description.
        let tier = crate::priority::tier_properties(
            crate::priority::Tier::Foreground,
            crate::priority::mem_total_bytes(),
        );
        let mut request = request;
        let at = request.iter().position(|a| a == "Description").expect("description");
        let tier_args: Vec<String> = tier
            .iter()
            .flat_map(crate::priority::dbus_property_args)
            .collect();
        assert_eq!(request[at - tier_args.len()..at], tier_args[..]);
        request.drain(at - tier_args.len()..at);
        assert_eq!(request[10], (5 + tier.len()).to_string());
        request[10] = "5".into();
        assert_eq!(
            request,
            [
                "--user",
                "--timeout=2",
                "call",
                "org.freedesktop.systemd1",
                "/org/freedesktop/systemd1",
                "org.freedesktop.systemd1.Manager",
                "StartTransientUnit",
                "ssa(sv)a(sa(sv))",
                "app-tilo-com.spotify.Client-42.scope",
                "fail",
                "5",
                "PIDs",
                "au",
                "1",
                "42",
                "Slice",
                "s",
                "app.slice",
                "CollectMode",
                "s",
                "inactive-or-failed",
                "PartOf",
                "as",
                "1",
                "tilo-graphical-session.target",
                "Description",
                "s",
                "Spotify",
                "0",
            ]
        );
        assert_eq!(
            unit_name("com.spotify.Client", 42).as_deref(),
            Some("app-tilo-com.spotify.Client-42.scope")
        );
        assert_eq!(
            unit_name("so.tilo.terminal", 7).as_deref(),
            Some("app-tilo-so.tilo.terminal-7.scope")
        );
        assert_eq!(
            unit_name("org.gnome.Text-Editor", 1).as_deref(),
            Some("app-tilo-org.gnome.Text\\x2dEditor-1.scope")
        );
        assert_eq!(
            unit_name("a b/c", 1).as_deref(),
            Some("app-tilo-a\\x20b\\x2fc-1.scope")
        );
        assert_eq!(unit_name("", 1), None);
        assert_eq!(unit_name(&"x".repeat(250), 1), None);
        assert!(cgroup_is_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-tilo-a\\x2db-7.scope\n",
            "app-tilo-a\\x2db-7.scope"
        ));
        assert!(!cgroup_is_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-cursor-1.scope\n",
            "app-tilo-a\\x2db-7.scope"
        ));
    }

    #[test]
    fn an_unreachable_or_refusing_manager_is_a_reason_not_a_panic() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("scratch directory");
        let stand_in = |name: &str, status: u8| {
            let path = dir.path().join(name);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.argv\"\n\
                     echo 'Failed to connect to user scope bus' >&2\nexit {status}\n"
                ),
            )
            .expect("write the stand-in busctl");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("make the stand-in executable");
            path
        };
        let pid = std::process::id();
        // Phase `a_refusal_keeps_the_managers_reason`.
        {
            let busctl = stand_in("refusing-busctl", 1);
            let error = join_with(&busctl, "so.tilo.notes", "Notes", Duration::from_millis(20))
                .expect_err("a refusal is reported");
            assert!(error.contains("user scope bus"), "{error}");
            let argv = std::fs::read_to_string(dir.path().join("refusing-busctl.argv"))
                .expect("the manager was asked");
            assert_eq!(
                argv.lines().map(str::to_string).collect::<Vec<_>>(),
                request_args(&format!("app-tilo-so.tilo.notes-{pid}.scope"), pid, "Notes")
            );
        }
        // Phase `an_accepted_scope_that_never_arrives_times_out`.
        {
            let busctl = stand_in("accepting-busctl", 0);
            let error = join_with(&busctl, "so.tilo.notes", "Notes", Duration::from_millis(20))
                .expect_err("the process never moved");
            assert!(error.contains("not joined"), "{error}");
        }
        // Phase `a_missing_front_end_is_reported`.
        {
            let error = join_with(
                &dir.path().join("absent-busctl"),
                "so.tilo.notes",
                "Notes",
                Duration::from_millis(20),
            )
            .expect_err("no busctl to run");
            assert!(error.contains("absent-busctl"), "{error}");
        }
    }
}
