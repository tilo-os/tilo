//! The peer as a user service: what keeps the person's workspaces mounted at
//! `~/Tilo` while this account is logged in, started again at the next login.
//!
//! A systemd user unit on Linux, a launchd agent on macOS, each running the
//! device's one holder ([`crate::mcp`], `tilo mcp --hold`). Where neither
//! answers (a container, a session without a user manager) the holder runs as
//! a process of its own instead, until the computer restarts.
//!
//! A command here never replaces a holder that is there because it answered
//! slowly or a connection failed: the peer may be inside a step over a
//! workspace's folder, and stopping it there loses that step. Whether one is
//! there is read from facts that need no answer — the holder's lock, and the
//! unit's state — and a command waits for the holder that is. Only a holder
//! that agreed to hand the device over (one of an older program, or one an
//! agent started where the service's can keep it) is replaced, and it steps
//! down by the holders' own hand-over protocol ([`crate::mcp::held`]).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

/// The unit's name, and the launchd agent's label.
const UNIT: &str = "tilo-peer.service";
const LABEL: &str = "so.tilo.peer";

/// How long a stop gives the holder to exit before it is killed. The holder's own waits add up
/// to less than this (`mcp::held`), so a stop ends by itself rather than being killed.
pub(crate) const STOP_TIMEOUT: Duration = Duration::from_secs(30);

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

/// How long a command waits for the holder it started to listen.
const LISTENING: Duration = Duration::from_secs(10);
/// How long a command waits for a holder that is there to answer: a holder inside a step over a
/// workspace's folder answers when that step ends, and nothing replaces it for being slow.
const ANSWERING: Duration = Duration::from_secs(30);
/// How long a unit that is active without a holder for this login is waited for: its holder is either
/// starting, or serving a login that ended (a login made again makes another device, and its
/// holder steps down by itself). Neither is stopped for it.
const SETTLING: Duration = Duration::from_secs(20);
/// How often it looks, or asks, whether that holder answers.
const POLL: Duration = Duration::from_millis(100);
/// How long a holder asked to hand the device over is waited for: it unmounts the folders and
/// closes the stores before its process ends, and the service starts once it is gone.
const HANDING_OVER: Duration = Duration::from_secs(30);

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

/// Start the peer from now on, and at every later login: install and start the
/// user service, else start the holder as a process of its own. A holder that
/// is there already is waited for, not replaced: only one that agreed to hand
/// the device over (one of an older program, or one an agent started where the
/// service's can keep it) is asked to step down, and the service's holder takes
/// its place.
///
/// # Errors
/// Neither started, or the holder that is there did not answer in time.
pub(crate) fn start() -> Result<Running> {
    let exe = std::env::current_exe().context("this program's path")?;
    let place = place()?;
    // The unit is written when it changed, so the next login starts this program. What it says
    // never starts or replaces a holder by itself: the facts below decide that.
    if let Err(why) = install(&exe) {
        tracing::debug!(%why, "no user service manager answers here");
        return start_process(&place);
    }
    start_service(&place)
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

/// What a command sees about the peer from outside, without waiting on an answer.
#[derive(Clone, Copy, Debug)]
struct Facts {
    /// The user service's unit is active.
    unit_active: bool,
    /// A live process holds the device's holder lock: the kernel drops it with the process, so a
    /// holder that ended never reads as one that is there.
    holder_alive: bool,
    /// What the holder answered when asked to hand the device over to a holder of this program as
    /// the service's: `Some(true)` when it steps down, `Some(false)` when it keeps the device, and
    /// `None` when it did not answer, as one inside a step over a workspace's folder does not.
    steps_down: Option<bool>,
}

/// What to do about the peer, from [`Facts`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    /// Nothing is there: start the service.
    Start,
    /// A holder is there: wait for it. A slow answer replaces nothing.
    LeaveRunning,
    /// The holder agreed to hand the device over: wait for it to let go, and start the service.
    HandOver,
}

/// [`Decision`] for `facts`: a holder that is there is never replaced because a connect failed or
/// an ask timed out, only because it said it steps down for this program.
fn decide(facts: Facts) -> Decision {
    if facts.steps_down == Some(true) {
        return Decision::HandOver;
    }
    if facts.holder_alive || facts.unit_active {
        return Decision::LeaveRunning;
    }
    Decision::Start
}

/// Start the service's holder when nothing is there, wait for the one that is, and take the
/// device only from a holder that agreed to hand it over. Its state is read again while a unit
/// that is active has no holder for this login: that holder is starting, or serving a login that
/// ended and stepping down by itself, and neither is a reason to stop it.
fn start_service(place: &crate::mcp::held::Place) -> Result<Running> {
    let mut settled = false;
    let mut settling_since: Option<Instant> = None;
    loop {
        let holder_alive = crate::mcp::held::holder_alive(place);
        if holder_alive {
            settling_since = None;
        }
        let steps_down = if holder_alive {
            crate::mcp::held::ask_to_step_down(place, service_claim())
        } else {
            None
        };
        let facts = Facts {
            unit_active: unit_active(),
            holder_alive,
            steps_down,
        };
        tracing::debug!(?facts, "what this command found of the peer");
        match decide(facts) {
            Decision::Start => {
                return match start_unit().and_then(|()| wait_listening(place)) {
                    Ok(()) => Ok(Running::Service),
                    Err(why) => {
                        // A user manager that answers but starts no holder: one of this process's
                        // own keeps the folders meanwhile, as where no manager answers at all.
                        tracing::debug!(%why, "the user service's holder did not start");
                        crate::mcp::held::ensure(place).map_err(anyhow::Error::msg)?;
                        Ok(Running::Process)
                    }
                };
            }
            Decision::LeaveRunning if facts.holder_alive => {
                // A holder whose socket for this login is not there is starting, or serves a
                // login that ended (a login made again makes another device, and its holder steps
                // down by itself). It is waited for, never stopped: once it listens it is asked,
                // and once it lets the lock go what to do is read again.
                if !crate::mcp::held::listening(place) && !settled {
                    settled = true;
                    if !settle(place) {
                        continue;
                    }
                }
                if wait_answering(place)? {
                    return Ok(if facts.unit_active {
                        Running::Service
                    } else {
                        Running::Process
                    });
                }
                settled = false;
            }
            Decision::LeaveRunning => {
                if settling_since.get_or_insert_with(Instant::now).elapsed() >= SETTLING {
                    bail!(
                        "the user service is active, but no holder of this computer's login \
                         is. `tilo status` again in a minute says how it stands."
                    );
                }
                std::thread::sleep(POLL);
            }
            Decision::HandOver => {
                wait_gone(place)?;
                start_after_handover()?;
                wait_listening(place)?;
                return Ok(Running::Service);
            }
        }
    }
}

/// Start the holder as a process of its own, there being no user service manager to start it:
/// one this login's commands reach is waited for, and where none listens a holder is started,
/// which takes the lock once a holder that is leaving lets go ([`crate::mcp::held::take_lock`]).
fn start_process(place: &crate::mcp::held::Place) -> Result<Running> {
    if crate::mcp::held::connect(place).is_some() && wait_answering(place)? {
        return Ok(Running::Process);
    }
    crate::mcp::held::ensure(place).map_err(anyhow::Error::msg)?;
    Ok(Running::Process)
}

/// The claim this command's holder would make: the user service's, of this program.
fn service_claim() -> crate::mcp::held::Claim {
    crate::mcp::held::Claim {
        program: crate::mcp::held::Program::running(),
        service: true,
    }
}

/// Wait until the holder listens at `place`: the service's holder enters the workspace before it
/// listens, so a start is waited for.
fn wait_listening(place: &crate::mcp::held::Place) -> Result<()> {
    let until = Instant::now()
        .checked_add(LISTENING)
        .unwrap_or_else(Instant::now);
    while Instant::now() < until {
        if crate::mcp::held::connect(place).is_some() {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
    bail!(
        "Tilo's background sync started but isn't listening on this computer yet. `tilo status` \
         again in a minute says how it stands."
    )
}

/// Wait, for at most [`SETTLING`], for the holder that holds the device's lock to listen for this
/// login: one that is starting does in a moment, and one that serves a login which ended never
/// does and lets the lock go by itself. `true` when it listens now; `false` when the lock was let
/// go, or the wait ran out.
fn settle(place: &crate::mcp::held::Place) -> bool {
    let until = Instant::now()
        .checked_add(SETTLING)
        .unwrap_or_else(Instant::now);
    while Instant::now() < until {
        if crate::mcp::held::listening(place) {
            return true;
        }
        if !crate::mcp::held::holder_alive(place) {
            return false;
        }
        std::thread::sleep(POLL);
    }
    crate::mcp::held::listening(place)
}

/// Wait for the holder that is there to answer, for at most [`ANSWERING`]: a command that could
/// not reach it quickly waits here, rather than replacing a peer that is busy with a large
/// change. `false` when it did not answer but nothing is there any more either, and what to do is
/// decided again.
fn wait_answering(place: &crate::mcp::held::Place) -> Result<bool> {
    let until = Instant::now()
        .checked_add(ANSWERING)
        .unwrap_or_else(Instant::now);
    let mut said = false;
    loop {
        if crate::mcp::ask_peer().is_some() {
            return Ok(true);
        }
        if !crate::mcp::held::holder_alive(place) && !unit_active() {
            return Ok(false);
        }
        if Instant::now() >= until {
            break;
        }
        if !said {
            said = true;
            eprintln!(
                "Tilo's background sync is busy with a large change on this computer; waiting \
                 for it to answer. It is left alone."
            );
        }
        std::thread::sleep(POLL);
    }
    bail!(
        "Tilo's background sync is still busy on this computer, so it did not answer. Nothing \
         was restarted: `tilo status` again in a minute says how it stands."
    )
}

/// Wait for the holder that was asked to hand the device over to let it go: the service starts
/// once neither the lock nor the unit says it is there.
fn wait_gone(place: &crate::mcp::held::Place) -> Result<()> {
    let until = Instant::now()
        .checked_add(HANDING_OVER)
        .unwrap_or_else(Instant::now);
    while Instant::now() < until {
        if !crate::mcp::held::holder_alive(place) && !unit_active() {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
    bail!(
        "the Tilo keeping this computer's workspaces did not hand them over in time. `tilo \
         status` again in a minute says how it stands."
    )
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
         Restart=on-failure\nRestartSec=5\nTimeoutStopSec={}\nEnvironment=\"{SERVICE_ENV}=1\"\n",
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

/// Write the unit for the next login when it changed, and see that it starts at every login.
/// Nothing here starts, stops or restarts the holder that is there: a unit file's change is no
/// reason to take the workspaces from a peer that is keeping them.
#[cfg(target_os = "linux")]
fn install(exe: &Path) -> Result<()> {
    // No user manager answering, nothing is written that one would start later.
    quiet(Command::new("systemctl").args(["--user", "is-system-running"]))
        .or_else(|_| quiet(Command::new("systemctl").args(["--user", "show-environment"])))?;
    let path = unit_path()?;
    let text = unit(exe, &carried());
    if !std::fs::read_to_string(&path).is_ok_and(|kept| kept == text) {
        std::fs::create_dir_all(path.parent().context("the unit's directory")?)?;
        std::fs::write(&path, &text)?;
        quiet(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
        quiet(Command::new("systemctl").args(["--user", "enable", UNIT]))?;
        return Ok(());
    }
    // There and unchanged: enabled for the next login, and left to serve as it does.
    if quiet(Command::new("systemctl").args(["--user", "is-enabled", "--quiet", UNIT])).is_err() {
        quiet(Command::new("systemctl").args(["--user", "enable", UNIT]))?;
    }
    Ok(())
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

/// Whether the unit is active.
#[cfg(target_os = "linux")]
fn unit_active() -> bool {
    quiet(Command::new("systemctl").args(["--user", "is-active", "--quiet", UNIT])).is_ok()
}

/// Start the unit: nothing when it is active already.
#[cfg(target_os = "linux")]
fn start_unit() -> Result<()> {
    quiet(Command::new("systemctl").args(["--user", "start", UNIT]))
}

/// Start the holder once the one that ran handed the device over. On macOS the job is loaded
/// again from the file on disk, which may name this program where the loaded job named an older
/// one; systemd was reloaded already, when the unit's file changed ([`install`]).
#[cfg(target_os = "macos")]
fn start_after_handover() -> Result<()> {
    let path = agent_path()?;
    let _ = quiet(Command::new("launchctl").args(["bootout", &format!("{}/{LABEL}", domain())]));
    quiet(
        Command::new("launchctl")
            .arg("bootstrap")
            .arg(domain())
            .arg(&path),
    )
}

#[cfg(not(target_os = "macos"))]
fn start_after_handover() -> Result<()> {
    start_unit()
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

/// Write the agent for the next login when it changed. A loaded agent keeps the program it was
/// loaded with, and loading it again here would stop the holder where it is, mid-change.
#[cfg(target_os = "macos")]
fn install(exe: &Path) -> Result<()> {
    let path = agent_path()?;
    let text = agent(exe, &carried());
    if std::fs::read_to_string(&path).is_ok_and(|kept| kept == text) {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().context("the agent's directory")?)?;
    std::fs::write(&path, &text)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn installed_for_this_login() -> bool {
    agent_path().is_ok_and(|path| path.exists())
}

/// Whether the agent is there: a loaded agent whose holder ended answers nothing, and is started
/// again rather than waited for.
#[cfg(target_os = "macos")]
fn unit_active() -> bool {
    Command::new("launchctl")
        .args(["list", LABEL])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .any(|line| line.trim_start().starts_with("\"PID\""))
        })
}

/// Start the agent: a loaded job's holder is started when none is there; else the job is loaded
/// from its file.
#[cfg(target_os = "macos")]
fn start_unit() -> Result<()> {
    let target = format!("{}/{LABEL}", domain());
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
fn install(_exe: &Path) -> Result<()> {
    bail!("no user service manager here")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn uninstall() {}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const fn installed_for_this_login() -> bool {
    false
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const fn unit_active() -> bool {
    false
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn start_unit() -> Result<()> {
    bail!("no user service manager here")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a command sees of the peer: whether the unit is active, whether a live process holds
    /// the device's lock, and what the holder answered when asked to hand the device over.
    const fn facts(unit_active: bool, holder_alive: bool, steps_down: Option<bool>) -> Facts {
        Facts {
            unit_active,
            holder_alive,
            steps_down,
        }
    }

    /// A peer that is there is waited for, never started again, however slowly it answers: the
    /// answer a command could not get says nothing about whether the peer is there.
    #[test]
    fn a_peer_that_runs_is_waited_for_and_never_started_again() {
        assert_eq!(
            decide(facts(false, false, None)),
            Decision::Start,
            "nothing is there: the service is started"
        );
        assert_eq!(
            decide(facts(true, true, None)),
            Decision::LeaveRunning,
            "a holder inside a step over a folder answers nothing, and is still there"
        );
        assert_eq!(
            decide(facts(false, true, None)),
            Decision::LeaveRunning,
            "a holder an agent started is there without the unit"
        );
        assert_eq!(
            decide(facts(true, false, None)),
            Decision::LeaveRunning,
            "the unit is active but its holder has not taken the lock yet: it is starting"
        );
        assert_eq!(
            decide(facts(true, true, Some(false))),
            Decision::LeaveRunning,
            "a holder of this program, or a newer one, keeps the device"
        );
    }

    /// Only a holder that said it steps down for this program is replaced, and the device is
    /// handed over rather than restarted: it leaves, then the service's holder starts.
    #[test]
    fn only_a_holder_that_handed_the_device_over_is_replaced() {
        assert_eq!(
            decide(facts(true, true, Some(true))),
            Decision::HandOver,
            "a holder of an older program steps down for this one"
        );
        assert_eq!(
            decide(facts(false, true, Some(true))),
            Decision::HandOver,
            "an agent's holder steps down for the service's, which keeps the device"
        );
    }

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
        assert!(
            unit.contains(&format!("TimeoutStopSec={}\n", STOP_TIMEOUT.as_secs())),
            "a stop is given its whole budget before the holder is killed: {unit}"
        );
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

    /// Installing writes the unit when it changed, and asks systemd nothing else: no start, no
    /// stop and above all no restart, so a holder that is there keeps the workspaces it keeps. What
    /// happens to that holder is [`decide`]'s, never an install's.
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

        install(Path::new("/opt/tilo")).unwrap();
        let written = std::fs::read_to_string(unit_path().unwrap()).unwrap();
        assert!(
            written.contains("ExecStart=\"/opt/tilo\" mcp --hold\n"),
            "{written}"
        );

        // The same unit again: systemd is asked whether it is enabled, and nothing else.
        std::fs::remove_file(&asked).unwrap();
        install(Path::new("/opt/tilo")).unwrap();
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
        install(Path::new("/opt/tilo-next")).unwrap();
        let changed = std::fs::read_to_string(&asked).unwrap();
        assert!(changed.contains("daemon-reload"), "{changed}");
        assert!(changed.contains("enable"), "{changed}");
        assert!(
            !changed.contains("start") && !changed.contains("stop"),
            "a changed unit is written for the next login: {changed}"
        );
    }
}
