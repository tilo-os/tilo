//! The one way this computer's daemon starts. On Tilo OS the system unit runs it from boot and
//! there is nothing to start. Everywhere else it is the signed-in person's own service, with no
//! administrator's rights: a systemd user unit on Linux, a launchd agent on macOS (Tilo.app's own
//! where it is loaded, else one of this login's). Every client that finds no daemon answering
//! calls [`start`]; none starts a daemon of its own.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{STOP_TIMEOUT, runtime_program, system_daemon};

/// What a client that could not reach the daemon tells the person.
pub const LOOK: &str = "run `tilo status` to see how it stands";

/// The unit's name, and the launchd agent's label.
const UNIT: &str = "tilo-peer.service";
const LABEL: &str = "so.tilo.peer";
/// Tilo.app's own launchd agent, which runs this computer's daemon where the app is installed.
#[cfg(target_os = "macos")]
const APP_LABEL: &str = "so.tilo.runtime";

/// The environment the service carries over from the login that installed it: where the device
/// is kept, and what a test's HTTPS trusts.
const CARRIED: &[&str] = &[
    "TILO_IDENTITY_ROOT",
    "TILO_TEST_CA_FILE",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
];

/// Start this computer's daemon: on Tilo OS nothing, the system unit runs it from boot;
/// elsewhere install the person's own service when it is missing or changed, and start it. A
/// daemon that runs already is left as it is.
///
/// # Errors
/// No service manager answers for this login, or it did not start the service.
pub fn start() -> Result<(), String> {
    if system_daemon() {
        return Ok(());
    }
    let program = runtime_program().map_err(|error| error.to_string())?;
    install(&program)
        .and_then(|()| start_unit())
        .map_err(|why| {
            format!(
                "Tilo's background service did not start on this computer ({why}). It runs as your \
             own service: on Linux a systemd user unit (`systemctl --user status {UNIT}`), on \
             macOS a launchd agent."
            )
        })
}

/// Stop this login's service and keep it from starting at the next login. The system unit is
/// the machine's, and left alone.
pub fn stop() {
    if !system_daemon() {
        uninstall();
    }
}

fn carried() -> Vec<(&'static str, String)> {
    CARRIED
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| (*name, value)))
        .collect()
}

/// The systemd user unit that runs `exe` as the daemon.
#[cfg_attr(target_os = "macos", allow(dead_code, reason = "Linux's service"))]
fn unit(exe: &Path, environment: &[(&str, String)]) -> String {
    let mut unit = format!(
        "[Unit]\nDescription=Tilo: your workspaces' files at ~/Tilo, kept in sync\n\
         After=network-online.target\n\n[Service]\nExecStart=\"{}\"\n\
         Restart=on-failure\nRestartSec=5\nTimeoutStopSec={}\n",
        exe.display(),
        STOP_TIMEOUT.as_secs()
    );
    for (name, value) in environment {
        let _ = writeln!(
            unit,
            "Environment=\"{name}={}\"",
            value.replace('"', "\\\"")
        );
    }
    unit.push_str("\n[Install]\nWantedBy=default.target\n");
    unit
}

/// The launchd agent that runs `exe` as the daemon.
#[cfg_attr(not(target_os = "macos"), allow(dead_code, reason = "macOS's service"))]
fn agent(exe: &Path, environment: &[(&str, String)]) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut variables = String::new();
    for (name, value) in environment {
        let _ = writeln!(
            variables,
            "    <key>{}</key><string>{}</string>",
            escape(name),
            escape(value)
        );
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>Label</key><string>{LABEL}</string>\n\
         <key>ProgramArguments</key><array><string>{}</string></array>\n\
         <key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n\
         <key>EnvironmentVariables</key><dict>\n{variables}</dict>\n\
         </dict></plist>\n",
        escape(&exe.display().to_string())
    )
}

fn quiet(command: &mut Command) -> Result<(), String> {
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("{command:?}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command:?} failed"))
    }
}

#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf, String> {
    Ok(tilo_paths::config_dir()
        .ok_or("this account has no configuration directory")?
        .join("systemd/user")
        .join(UNIT))
}

#[cfg(target_os = "linux")]
fn systemctl(args: &[&str]) -> Result<(), String> {
    quiet(Command::new("systemctl").arg("--user").args(args))
}

/// Write the unit when it changed, and see that it starts at every login. Nothing here stops or
/// restarts the daemon that is there: a unit file's change is no reason to take the workspaces
/// from a daemon that is keeping them.
#[cfg(target_os = "linux")]
fn install(exe: &Path) -> Result<(), String> {
    systemctl(&["show-environment"])?;
    let path = unit_path()?;
    let text = unit(exe, &carried());
    if !std::fs::read_to_string(&path).is_ok_and(|kept| kept == text) {
        let dir = path.parent().ok_or("the unit's directory")?;
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        std::fs::write(&path, &text).map_err(|error| error.to_string())?;
        systemctl(&["daemon-reload"])?;
        return systemctl(&["enable", UNIT]);
    }
    if systemctl(&["is-enabled", "--quiet", UNIT]).is_err() {
        systemctl(&["enable", UNIT])?;
    }
    Ok(())
}

/// Start the unit: nothing when it is active already.
#[cfg(target_os = "linux")]
fn start_unit() -> Result<(), String> {
    systemctl(&["start", UNIT])
}

#[cfg(target_os = "linux")]
fn uninstall() {
    if let Ok(path) = unit_path()
        && path.exists()
    {
        let _ = systemctl(&["disable", "--now", UNIT]);
        let _ = std::fs::remove_file(path);
        let _ = systemctl(&["daemon-reload"]);
    }
}

#[cfg(target_os = "macos")]
fn agent_path() -> Result<PathBuf, String> {
    Ok(tilo_paths::home()
        .ok_or("HOME is missing")?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

/// This login's launchd domain, by the account that owns its home.
#[cfg(target_os = "macos")]
fn domain() -> String {
    use std::os::unix::fs::MetadataExt as _;
    let uid = tilo_paths::home()
        .and_then(|home| std::fs::metadata(home).ok())
        .map_or(0, |home| home.uid());
    format!("gui/{uid}")
}

/// Whether Tilo.app's own agent is loaded: it runs this computer's daemon, and no second agent
/// is installed beside it.
#[cfg(target_os = "macos")]
fn app_agent() -> bool {
    quiet(Command::new("launchctl").args(["print", &format!("{}/{APP_LABEL}", domain())])).is_ok()
}

/// Write the agent when it changed. A loaded agent keeps the program it was loaded with, and
/// loading it again here would stop the daemon where it is, mid-change.
#[cfg(target_os = "macos")]
fn install(exe: &Path) -> Result<(), String> {
    if app_agent() {
        return Ok(());
    }
    let path = agent_path()?;
    let text = agent(exe, &carried());
    if std::fs::read_to_string(&path).is_ok_and(|kept| kept == text) {
        return Ok(());
    }
    let dir = path.parent().ok_or("the agent's directory")?;
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    std::fs::write(&path, &text).map_err(|error| error.to_string())
}

/// Start the agent: a loaded job's daemon is started when none is there; else the job is loaded
/// from its file.
#[cfg(target_os = "macos")]
fn start_unit() -> Result<(), String> {
    let label = if app_agent() { APP_LABEL } else { LABEL };
    let target = format!("{}/{label}", domain());
    if quiet(Command::new("launchctl").args(["print", &target])).is_ok() {
        return quiet(Command::new("launchctl").args(["kickstart", &target]));
    }
    let path = agent_path()?;
    quiet(
        Command::new("launchctl")
            .arg("bootstrap")
            .arg(domain())
            .arg(&path),
    )
}

#[cfg(target_os = "macos")]
fn uninstall() {
    if let Ok(path) = agent_path()
        && path.exists()
    {
        let _ =
            quiet(Command::new("launchctl").args(["bootout", &format!("{}/{LABEL}", domain())]));
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn install(_exe: &Path) -> Result<(), String> {
    Err("no service manager here".to_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn start_unit() -> Result<(), String> {
    Err("no service manager here".to_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn uninstall() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_runs_the_daemon_with_the_logins_environment() {
        let unit = unit(
            Path::new("/home/ada/.local/bin/tilo-runtime"),
            &[(
                "TILO_IDENTITY_ROOT",
                "/home/ada/state/tilo/identity".to_owned(),
            )],
        );
        assert!(
            unit.contains("ExecStart=\"/home/ada/.local/bin/tilo-runtime\"\n"),
            "{unit}"
        );
        assert!(
            unit.contains("Environment=\"TILO_IDENTITY_ROOT=/home/ada/state/tilo/identity\"\n")
        );
        assert!(
            unit.contains(&format!("TimeoutStopSec={}\n", STOP_TIMEOUT.as_secs())),
            "a stop is given its whole budget before the daemon is killed: {unit}"
        );
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn the_launchd_agent_runs_the_daemon_and_escapes_what_it_names() {
        let agent = agent(
            Path::new("/Users/you&co/tilo-runtime"),
            &[("XDG_STATE_HOME", "/s<t>".to_owned())],
        );
        assert!(agent.contains("<string>/Users/you&amp;co/tilo-runtime</string></array>"));
        assert!(agent.contains("<key>XDG_STATE_HOME</key><string>/s&lt;t&gt;</string>"));
    }

    /// Installing writes the unit when it changed, and asks systemd nothing else: no start, no
    /// stop and above all no restart, so a daemon that is there keeps the workspaces it keeps.
    #[cfg(target_os = "linux")]
    #[test]
    fn installing_never_starts_or_restarts_the_unit() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = tempfile::tempdir().unwrap();
        let commands = tempfile::tempdir().unwrap();
        let asked = home.path().join("systemctl.calls");
        // A stand-in for systemd that logs what it was asked, and says yes to all of it.
        let systemctl = commands.path().join("systemctl");
        std::fs::write(
            &systemctl,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
                asked.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("PATH", commands.path())
            .set("HOME", home.path())
            .set("XDG_CONFIG_HOME", home.path());

        install(Path::new("/opt/tilo-runtime")).unwrap();
        let written = std::fs::read_to_string(unit_path().unwrap()).unwrap();
        assert!(
            written.contains("ExecStart=\"/opt/tilo-runtime\"\n"),
            "{written}"
        );

        // The same unit again: systemd is asked whether it is enabled, and nothing else.
        std::fs::remove_file(&asked).unwrap();
        install(Path::new("/opt/tilo-runtime")).unwrap();
        let again = std::fs::read_to_string(&asked).unwrap();
        assert!(again.contains("is-enabled"), "{again}");
        assert!(
            !again.contains("daemon-reload"),
            "the unit did not change: {again}"
        );
        assert!(
            !again.contains("start") && !again.contains("stop"),
            "an unchanged unit is left alone: {again}"
        );

        // A changed unit is written and reloaded, and still nothing starts or stops.
        std::fs::remove_file(&asked).unwrap();
        install(Path::new("/opt/tilo-next/tilo-runtime")).unwrap();
        let changed = std::fs::read_to_string(&asked).unwrap();
        assert!(changed.contains("daemon-reload"), "{changed}");
        assert!(changed.contains("enable"), "{changed}");
        assert!(
            !changed.contains("start") && !changed.contains("stop"),
            "a changed unit is written for the next login: {changed}"
        );
    }
}
