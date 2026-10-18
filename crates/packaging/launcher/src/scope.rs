//! Asking the systemd user manager for a scope.
//!
//! Every launch runs in a cgroup of its own, in `app.slice` and part of the
//! graphical session target. That makes an app one unit for resource
//! accounting (its memory is charged to its own cgroup, never the
//! compositor's), for the OOM killer, and for logout or a compositor crash,
//! which stop the app and everything it forked instead of leaving it running
//! with no display.
//!
//! The compositor's launches get that cgroup from the session's app spawner
//! ([`crate::spawner`]), born inside it, with no round trip: the spawner
//! asks the manager once per compositor run for a scope that delegates its
//! subtree ([`join_app_tree_with`], see [`crate::app_cgroup`]). A launcher
//! started some other way — by a root helper, from a terminal, or while the
//! spawner is not up yet — still asks for its own transient
//! `app-tilo-<id>-<pid>.scope` ([`join`]) and moves *itself* into it before
//! it forks the app, so the app and every process it starts are inside it
//! rather than chased into it afterwards.
//!
//! Either way `OOMPolicy` is `continue`: systemd never tears a unit down on
//! its own when the kernel's OOM killer acts in it. What ends together is
//! decided inside the delegated tree instead ([`crate::app_cgroup`]): a Tilo
//! app's main leaf is one OOM group, so an app that runs out of memory ends
//! completely, while each independent job it started (a terminal session, a
//! build, a language server) is an OOM group of its own beside it and ends
//! alone. A host app keeps one victim. A launcher in a scope of its own (the
//! fallback) gets no grouping: its scope is not delegated. systemd-oomd never
//! acts on apps: `app.slice` is not one of its targets (see
//! `os/etc/systemd/user/app.slice.d`); the session's app spawner watches
//! memory pressure for them ([`crate::memory_pressure`]).
//!
//! A manager that is unreachable (a headless test host, a session without a
//! user manager) or refuses costs the scope, never the launch: the caller
//! logs the reason and runs the app where the launcher already is.

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

/// What happens to an app's scope when the kernel OOM killer kills one of
/// its processes: only that process dies (see the module docs).
const OOM_POLICY: &str = "continue";

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
    let escaped = escape_name(app_id)?;
    let unit = format!("app-tilo-{escaped}-{pid}.scope");
    (unit.len() <= UNIT_NAME_MAX).then_some(unit)
}

/// `name` escaped as systemd escapes a unit name's field: `-` separates
/// fields, so it and anything else outside `[A-Za-z0-9:_.]` becomes `\xNN`.
/// `None` for an empty name. The delegated app cgroups use the same spelling
/// (`crate::app_cgroup`), so an app reads alike in either.
#[must_use]
pub fn escape_name(name: &str) -> Option<String> {
    use std::fmt::Write as _;
    let mut escaped = String::with_capacity(name.len());
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'.') {
            escaped.push(char::from(byte));
        } else {
            let _ = write!(escaped, "\\x{byte:02x}");
        }
    }
    (!escaped.is_empty()).then_some(escaped)
}

/// The inverse of [`escape_name`]. Anything that is not a well-formed
/// `\xNN` stays as it is.
#[must_use]
pub fn unescape_name(escaped: &str) -> String {
    let bytes = escaped.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let hex = bytes
            .get(at..at.saturating_add(4))
            .filter(|run| run.starts_with(b"\\x"))
            .and_then(|run| std::str::from_utf8(run.get(2..)?).ok())
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        if let Some(decoded) = hex {
            out.push(decoded);
            at = at.saturating_add(4);
        } else {
            out.push(byte);
            at = at.saturating_add(1);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
    join_unit(
        busctl,
        &unit,
        &request_args(&unit, pid, description),
        timeout,
    )
}

/// Run one `StartTransientUnit` call of a scope that holds this process and
/// wait until the move into `unit` is visible in `/proc/self/cgroup`.
fn join_unit(
    busctl: &Path,
    unit: &str,
    args: &[String],
    timeout: Duration,
) -> Result<String, String> {
    // `StartTransientUnit` answers once the start job is queued, before the
    // move, hence the wait on this process's own cgroup below.
    let output = Command::new(busctl)
        .args(args)
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
        if cgroup_is_unit(&cgroup, unit) {
            return Ok(unit.to_string());
        }
        if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
            return Err(format!(
                "{unit} was accepted but not joined within {timeout:?}"
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Move this process, the session's app spawner, into a new scope that
/// delegates its cgroup subtree: `tilo-apps-<pid>.scope` in `app.slice`
/// (see [`crate::app_cgroup`]). One call per compositor run, made before the
/// first launch needs it; every app after that gets its cgroup from the
/// spawner without asking the manager anything. Returns the unit name.
///
/// # Errors
///
/// As [`join`].
pub fn join_app_tree_with(busctl: &Path, timeout: Duration) -> Result<String, String> {
    let pid = std::process::id();
    let unit = crate::app_cgroup::tree_unit_name(pid);
    join_unit(busctl, &unit, &tree_request_args(&unit, pid), timeout)
}

/// `busctl` arguments for `StartTransientUnit` of the delegated app tree
/// holding the spawner `pid`.
///
/// What each app scope was given becomes either the tree's own setting or a
/// file the spawner writes in the app's cgroup: the slice, the session link
/// and `OOMPolicy=continue` are the tree's (systemd sees one unit, so with
/// `stop` one process killed for memory in any app would stop every app);
/// the tier weights and floor are per app, in its cgroup. The tree itself
/// carries the foreground weights, as the focused app's scope did, so what
/// the person looks at still outweighs the rest of `app.slice`, and the
/// foreground floor, so the focused app's own floor has protection to draw
/// on (`memory_recursiveprot`).
#[must_use]
pub fn tree_request_args(unit: &str, pid: u32) -> Vec<String> {
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
        vec!["OOMPolicy".into(), "s".into(), OOM_POLICY.into()],
        // The subtree is the spawner's: systemd neither creates nor removes
        // cgroups below the scope, and hands it every controller the user
        // manager has (`os/etc/systemd/system/user@.service.d`).
        vec!["Delegate".into(), "b".into(), "true".into()],
    ];
    let tier = crate::priority::tier_properties(
        crate::priority::Tier::Foreground,
        crate::priority::mem_total_bytes(),
    );
    properties.extend(
        tier.iter()
            .map(|p| crate::priority::dbus_property_args(p).to_vec()),
    );
    properties.push(vec!["Description".into(), "s".into(), "Tilo apps".into()]);
    start_args(unit, properties)
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
        vec!["OOMPolicy".into(), "s".into(), OOM_POLICY.into()],
    ];
    // The person just asked for this app: it starts in the foreground tier,
    // and the compositor demotes it when focus moves on (see `priority`).
    let tier = crate::priority::tier_properties(
        crate::priority::Tier::Foreground,
        crate::priority::mem_total_bytes(),
    );
    properties.extend(
        tier.iter()
            .map(|p| crate::priority::dbus_property_args(p).to_vec()),
    );
    let description: String = description.chars().filter(|c| !c.is_control()).collect();
    if !description.trim().is_empty() {
        properties.push(vec!["Description".into(), "s".into(), description]);
    }
    start_args(unit, properties)
}

/// `busctl --user call … StartTransientUnit` of `unit` with `properties`,
/// each a `name signature value…` run.
fn start_args(unit: &str, properties: Vec<Vec<String>>) -> Vec<String> {
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
        let at = request
            .iter()
            .position(|a| a == "Description")
            .expect("description");
        let tier_args: Vec<String> = tier
            .iter()
            .flat_map(crate::priority::dbus_property_args)
            .collect();
        assert_eq!(request[at - tier_args.len()..at], tier_args[..]);
        request.drain(at - tier_args.len()..at);
        assert_eq!(request[10], (6 + tier.len()).to_string());
        request[10] = "6".into();
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
                "6",
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
                // One process killed for memory must not take down the app.
                "OOMPolicy",
                "s",
                "continue",
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
