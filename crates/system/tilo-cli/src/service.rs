//! The peer as a user service: what keeps the person's workspaces mounted at
//! `~/Tilo` while this account is logged in, started again at the next login.
//!
//! A systemd user unit on Linux, a launchd agent on macOS, each running the
//! device's one holder ([`crate::mcp`], `tilo mcp --hold`). Where neither
//! answers (a container, a session without a user manager) the holder runs as
//! a process of its own instead, until the computer restarts.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result};

/// The unit's name, and the launchd agent's label.
const UNIT: &str = "tilo-peer.service";
const LABEL: &str = "so.tilo.peer";

/// The environment the service carries over from the login that installed
/// it: where the device is kept, and what a test's HTTPS trusts.
const CARRIED: &[&str] = &[
    "TILO_DEVICE_DIR",
    crate::login::TEST_CA_ENV,
    "XDG_STATE_HOME",
];

/// Set in the service's environment: the holder it starts knows itself as the service's, the one
/// that lasts for as long as the person is logged in ([`crate::mcp::held::Claim`]).
const SERVICE_ENV: &str = "TILO_PEER_SERVICE";

/// Whether this process is the holder the user service started.
pub(crate) fn running_as_service() -> bool {
    std::env::var_os(SERVICE_ENV).is_some_and(|value| value == "1")
}

/// Start the installed user service, when it is installed for this login: what an agent's or a
/// command's `tilo` does when no holder listens, so the device's holder is the service's rather
/// than a process of theirs. `false` when there is no service for this login, or it did not
/// start, and the caller starts a holder itself.
pub(crate) fn start_installed() -> bool {
    installed_for_this_login() && start_unit().is_ok()
}

/// How the peer runs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Running {
    /// As the user service.
    Service,
    /// As a process of its own, there being no user service manager to run it.
    Process,
}

/// Run the peer from now on, and at every later login: install and start the
/// user service, else start the holder as a process of its own.
///
/// # Errors
/// Neither started.
pub(crate) fn start() -> Result<Running> {
    let exe = std::env::current_exe().context("this program's path")?;
    if install(&exe).is_ok() {
        // The service's holder listens once it entered: wait for it.
        if let Ok(place) = place() {
            for _ in 0..100 {
                if crate::mcp::held::connect(&place).is_some() {
                    return Ok(Running::Service);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    let place = place()?;
    crate::mcp::held::ensure(&place).map_err(anyhow::Error::msg)?;
    Ok(Running::Process)
}

/// Stop the peer, unmounting the workspaces, and keep it from starting at the
/// next login.
///
/// # Errors
/// The running peer could not be told to stop.
pub(crate) fn stop() -> Result<()> {
    uninstall();
    if let Some(status) = crate::mcp::ask_peer()
        && let Some(pid) = status.get("pid").and_then(serde_json::Value::as_u64)
        && let Ok(pid) = i32::try_from(pid)
    {
        let pid = rustix::process::Pid::from_raw(pid).context("the peer's process id")?;
        rustix::process::kill_process(pid, rustix::process::Signal::TERM)
            .context("telling the peer to stop")?;
    }
    Ok(())
}

fn place() -> Result<crate::mcp::held::Place> {
    let dir = crate::login::device_dir()?;
    let (key, _) = crate::login::load(&dir)?.context(crate::login::NOT_LOGGED_IN)?;
    crate::mcp::held::Place::of(&dir, &key.verifying_key().to_bytes()).map_err(anyhow::Error::msg)
}

fn carried() -> Vec<(&'static str, String)> {
    CARRIED
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| (*name, value)))
        .collect()
}

/// The systemd user unit that runs `exe` as the peer.
#[cfg_attr(target_os = "macos", allow(dead_code, reason = "Linux's service"))]
fn unit(exe: &Path, environment: &[(&str, String)]) -> String {
    let mut unit = format!(
        "[Unit]\nDescription=Tilo: your workspaces' files at ~/Tilo, kept in sync\n\
         After=network-online.target\n\n[Service]\nExecStart=\"{}\" mcp --hold\n\
         Restart=on-failure\nRestartSec=5\nEnvironment=\"{SERVICE_ENV}=1\"\n",
        exe.display()
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

/// The launchd agent that runs `exe` as the peer.
#[cfg_attr(not(target_os = "macos"), allow(dead_code, reason = "macOS's service"))]
fn agent(exe: &Path, environment: &[(&str, String)]) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut variables = format!("    <key>{SERVICE_ENV}</key><string>1</string>\n");
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
         <key>ProgramArguments</key><array><string>{}</string><string>mcp</string><string>--hold</string></array>\n\
         <key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n\
         <key>EnvironmentVariables</key><dict>\n{variables}</dict>\n\
         </dict></plist>\n",
        escape(&exe.display().to_string())
    )
}

fn quiet(command: &mut Command) -> Result<()> {
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    anyhow::ensure!(status.success(), "{command:?} failed");
    Ok(())
}

#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf> {
    Ok(tilo_paths::config_dir()
        .context("this account has no configuration directory")?
        .join("systemd/user")
        .join(UNIT))
}

#[cfg(target_os = "linux")]
fn install(exe: &Path) -> Result<()> {
    // No user manager answering, nothing is written that one would start later.
    quiet(Command::new("systemctl").args(["--user", "is-system-running"]))
        .or_else(|_| quiet(Command::new("systemctl").args(["--user", "show-environment"])))?;
    let path = unit_path()?;
    std::fs::create_dir_all(path.parent().context("the unit's directory")?)?;
    std::fs::write(&path, unit(exe, &carried()))?;
    quiet(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
    quiet(Command::new("systemctl").args(["--user", "enable", "--now", UNIT]))?;
    // A peer already running an older program is replaced by this one.
    quiet(Command::new("systemctl").args(["--user", "restart", UNIT]))
}

/// Whether this login's service is installed: its unit is there, carrying this command's own
/// device directory, so the holder it starts is this device's.
#[cfg(target_os = "linux")]
fn installed_for_this_login() -> bool {
    let Ok(text) = unit_path().and_then(|path| Ok(std::fs::read_to_string(path)?)) else {
        return false;
    };
    let carried_by_unit: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("Environment="))
        .filter(|line| {
            CARRIED.iter().any(|name| {
                line.trim_start_matches('"')
                    .starts_with(&format!("{name}="))
            })
        })
        .collect();
    let expected = unit(Path::new(""), &carried());
    let carried_here: Vec<&str> = expected
        .lines()
        .filter_map(|line| line.strip_prefix("Environment="))
        .filter(|line| !line.contains(SERVICE_ENV))
        .collect();
    carried_by_unit == carried_here
}

#[cfg(target_os = "linux")]
fn start_unit() -> Result<()> {
    quiet(Command::new("systemctl").args(["--user", "start", UNIT]))
}

#[cfg(target_os = "linux")]
fn uninstall() {
    if let Ok(path) = unit_path()
        && path.exists()
    {
        let _ = quiet(Command::new("systemctl").args(["--user", "disable", "--now", UNIT]));
        let _ = std::fs::remove_file(path);
        let _ = quiet(Command::new("systemctl").args(["--user", "daemon-reload"]));
    }
}

#[cfg(target_os = "macos")]
fn agent_path() -> Result<PathBuf> {
    Ok(tilo_paths::home()
        .context("HOME is missing")?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn domain() -> String {
    format!("gui/{}", rustix::process::getuid().as_raw())
}

#[cfg(target_os = "macos")]
fn install(exe: &Path) -> Result<()> {
    let path = agent_path()?;
    std::fs::create_dir_all(path.parent().context("the agent's directory")?)?;
    std::fs::write(&path, agent(exe, &carried()))?;
    let _ = quiet(Command::new("launchctl").args(["bootout", &format!("{}/{LABEL}", domain())]));
    quiet(
        Command::new("launchctl")
            .arg("bootstrap")
            .arg(domain())
            .arg(&path),
    )
}

#[cfg(target_os = "macos")]
fn installed_for_this_login() -> bool {
    agent_path().is_ok_and(|path| path.exists())
}

#[cfg(target_os = "macos")]
fn start_unit() -> Result<()> {
    quiet(Command::new("launchctl").args(["kickstart", &format!("{}/{LABEL}", domain())]))
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
fn install(_exe: &Path) -> Result<()> {
    anyhow::bail!("no user service manager here")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn uninstall() {}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const fn installed_for_this_login() -> bool {
    false
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn start_unit() -> Result<()> {
    anyhow::bail!("no user service manager here")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_runs_the_holder_with_the_logins_environment() {
        let unit = unit(
            Path::new("/home/ada/.local/bin/tilo"),
            &[("TILO_DEVICE_DIR", "/home/ada/state/tilo/device".to_owned())],
        );
        assert!(
            unit.contains("ExecStart=\"/home/ada/.local/bin/tilo\" mcp --hold\n"),
            "{unit}"
        );
        assert!(unit.contains("Environment=\"TILO_DEVICE_DIR=/home/ada/state/tilo/device\"\n"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(
            unit.contains("Environment=\"TILO_PEER_SERVICE=1\"\n"),
            "the holder it starts knows itself as the service's: {unit}"
        );
    }

    #[test]
    fn the_launchd_agent_runs_the_holder_and_escapes_what_it_names() {
        let agent = agent(
            Path::new("/Users/you&co/tilo"),
            &[("XDG_STATE_HOME", "/s<t>".to_owned())],
        );
        assert!(agent.contains(
            "<string>/Users/you&amp;co/tilo</string><string>mcp</string><string>--hold</string>"
        ));
        assert!(agent.contains("<key>XDG_STATE_HOME</key><string>/s&lt;t&gt;</string>"));
        assert!(agent.contains("<key>TILO_PEER_SERVICE</key><string>1</string>"));
    }
}
