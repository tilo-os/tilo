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
         Restart=on-failure\nRestartSec=5\n",
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
    }
}
