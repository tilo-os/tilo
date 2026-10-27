//! Open authorization pages in the signed, installed Browser under Cage.
//!
//! The installer and Browser run as greetd, never root. `tilo-launcher exec`
//! selects the installed package and applies its ordinary runtime verification,
//! bubblewrap, Landlock and seccomp policy. Browser's existing `net.mode = host`
//! shares the installer's network namespace: tilo-oauth's canonical loopback
//! listener is reachable without a callback proxy or additional capability.
//!
//! Keep this opener alive for the account actor's lifetime. Opening another
//! request replaces its previous Browser; dropping it closes that Browser.

use std::ffi::OsString;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

const LAUNCHER: &str = "/usr/libexec/tilo/tilo-launcher";
const PACKAGE: &str = "so.tilo.browser";
const MAX_URL_BYTES: usize = 16 * 1024;
const STARTUP_GRACE: Duration = Duration::from_millis(750);
const OPEN_DEADLINE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(25);

/// Synchronous boundary for an account actor; implementations must not log URLs.
pub trait BrowserOpener: Send + Sync {
    /// Hand an authorization URL to Browser, without waiting for authorization.
    ///
    /// # Errors
    /// A bounded, non-secret description of a launch failure.
    fn open(&self, url: &str) -> Result<(), BrowserError>;

    /// Return focus to the installer after callback, cancellation or failure.
    /// Test openers without a window need not implement this.
    ///
    /// # Errors
    /// A bounded launch-supervisor failure.
    fn close(&self) -> Result<(), BrowserError> {
        Ok(())
    }
}

/// Deliberately contains neither URLs, child output nor underlying I/O errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserError {
    Privileged,
    InvalidUrl,
    DisplayUnavailable,
    Unavailable,
    Busy,
    LaunchFailed,
    TimedOut,
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Privileged => "Browser authorization requires an unprivileged session",
            Self::InvalidUrl => "Invalid Browser authorization URL",
            Self::DisplayUnavailable => "Installer display is unavailable",
            Self::Unavailable => "Browser launcher is unavailable",
            Self::Busy => "Browser launcher is busy",
            Self::LaunchFailed => "Browser could not start",
            Self::TimedOut => "Browser launch did not respond in time",
        })
    }
}

impl std::error::Error for BrowserError {}

/// One supervised Browser, with one bounded pending request and no child logs.
///
/// No shell, host browser, URI portal, `xdg-open`, sudo or fallback is used.
/// Successful `open` means the launcher survived the startup grace period, not
/// that a page rendered or that OAuth completed. The account actor must still
/// await tilo-oauth's state-checked callback and handle its deadline/cancellation.
pub struct LiveBrowserOpener {
    requests: SyncSender<Request>,
}

enum Request {
    Open(OpenRequest),
    Close(SyncSender<Result<(), BrowserError>>),
}

struct OpenRequest {
    url: String,
    deadline: Instant,
    reply: SyncSender<Result<(), BrowserError>>,
}

impl LiveBrowserOpener {
    /// Capture the current Cage session and start its unprivileged supervisor.
    ///
    /// # Errors
    /// Refuses root, missing/unowned display sockets and thread creation failure.
    pub fn new() -> Result<Self, BrowserError> {
        unprivileged()?;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or(BrowserError::DisplayUnavailable)?;
        let display =
            std::env::var_os("WAYLAND_DISPLAY").ok_or(BrowserError::DisplayUnavailable)?;
        let socket = display_socket(&runtime, &display, rustix::process::geteuid().as_raw())?;
        Self::start(LaunchSession {
            socket,
            environment: session_environment(),
        })
    }

    fn start(session: LaunchSession) -> Result<Self, BrowserError> {
        let (requests, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("installer-browser".into())
            .spawn(move || supervise(&receiver, &session))
            .map_err(|_| BrowserError::Unavailable)?;
        Ok(Self { requests })
    }
}

impl BrowserOpener for LiveBrowserOpener {
    fn open(&self, url: &str) -> Result<(), BrowserError> {
        unprivileged()?;
        validate_url(url)?;
        let (reply, response) = mpsc::sync_channel(1);
        self.requests
            .try_send(Request::Open(OpenRequest {
                url: url.to_owned(),
                deadline: Instant::now()
                    .checked_add(OPEN_DEADLINE)
                    .ok_or(BrowserError::TimedOut)?,
                reply,
            }))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => BrowserError::Busy,
                mpsc::TrySendError::Disconnected(_) => BrowserError::Unavailable,
            })?;
        response
            .recv_timeout(OPEN_DEADLINE)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => BrowserError::TimedOut,
                mpsc::RecvTimeoutError::Disconnected => BrowserError::Unavailable,
            })?
    }

    fn close(&self) -> Result<(), BrowserError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.requests
            .try_send(Request::Close(reply))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => BrowserError::Busy,
                mpsc::TrySendError::Disconnected(_) => BrowserError::Unavailable,
            })?;
        response
            .recv_timeout(OPEN_DEADLINE)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => BrowserError::TimedOut,
                mpsc::RecvTimeoutError::Disconnected => BrowserError::Unavailable,
            })?
    }
}

fn unprivileged() -> Result<(), BrowserError> {
    // Check both: a setuid caller must not start an authorization session either.
    if rustix::process::getuid().is_root() || rustix::process::geteuid().is_root() {
        Err(BrowserError::Privileged)
    } else {
        Ok(())
    }
}

fn validate_url(value: &str) -> Result<(), BrowserError> {
    if value.len() > MAX_URL_BYTES || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(BrowserError::InvalidUrl);
    }
    let url = url::Url::parse(value).map_err(|_| BrowserError::InvalidUrl)?;
    let loopback = matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    {
        return Err(BrowserError::InvalidUrl);
    }
    Ok(())
}

fn display_socket(
    runtime: &Path,
    display: &std::ffi::OsStr,
    uid: u32,
) -> Result<PathBuf, BrowserError> {
    let metadata =
        std::fs::symlink_metadata(runtime).map_err(|_| BrowserError::DisplayUnavailable)?;
    if !runtime.is_absolute()
        || !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.mode() & 0o022 != 0
    {
        return Err(BrowserError::DisplayUnavailable);
    }
    let socket = runtime.join(display);
    let metadata =
        std::fs::symlink_metadata(&socket).map_err(|_| BrowserError::DisplayUnavailable)?;
    if !metadata.file_type().is_socket() || metadata.uid() != uid {
        return Err(BrowserError::DisplayUnavailable);
    }
    Ok(socket)
}

struct LaunchSession {
    socket: PathBuf,
    environment: Vec<(OsString, OsString)>,
}

fn session_environment() -> Vec<(OsString, OsString)> {
    // Do not inherit launcher-registration gates, developer package/runtime
    // overrides, browser flags, proxy credentials or arbitrary loader settings.
    [
        "HOME",
        "XDG_RUNTIME_DIR",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
    ]
    .into_iter()
    .filter_map(|name| std::env::var_os(name).map(|value| (name.into(), value)))
    .collect()
}

fn launch_command(session: &LaunchSession, url: &str) -> Command {
    let mut command = Command::new(LAUNCHER);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .envs(session.environment.iter().cloned())
        .args(["exec", "--package", PACKAGE, "--wayland-socket"])
        .arg(&session.socket)
        .arg("--open-uri")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command
}

fn supervise(requests: &Receiver<Request>, session: &LaunchSession) {
    let mut browser = None;
    loop {
        match requests.recv_timeout(POLL) {
            Ok(Request::Close(reply)) => {
                stop(&mut browser);
                let _ = reply.send(Ok(()));
            }
            Ok(Request::Open(request)) => {
                if Instant::now() >= request.deadline {
                    let _ = request.reply.send(Err(BrowserError::TimedOut));
                    continue;
                }
                stop(&mut browser);
                if Instant::now() >= request.deadline {
                    let _ = request.reply.send(Err(BrowserError::TimedOut));
                    continue;
                }
                let result = launch_command(session, &request.url)
                    .spawn()
                    .map_err(|_| BrowserError::Unavailable)
                    .and_then(|child| {
                        browser = Some(child);
                        startup(&mut browser, request.deadline)
                    });
                // The actor may have disappeared while launch was in progress.
                if request.reply.send(result).is_err() || result.is_err() {
                    stop(&mut browser);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(child) = browser.as_mut()
                    && child.try_wait().is_ok_and(|status| status.is_some())
                {
                    browser = None;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    stop(&mut browser);
}

fn startup(browser: &mut Option<Child>, deadline: Instant) -> Result<(), BrowserError> {
    let until = Instant::now()
        .checked_add(STARTUP_GRACE)
        .ok_or(BrowserError::TimedOut)?;
    let child = browser.as_mut().ok_or(BrowserError::LaunchFailed)?;
    loop {
        if child
            .try_wait()
            .map_err(|_| BrowserError::LaunchFailed)?
            .is_some()
        {
            return Err(BrowserError::LaunchFailed);
        }
        if Instant::now() >= deadline {
            return Err(BrowserError::TimedOut);
        }
        if Instant::now() >= until {
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
}

fn stop(browser: &mut Option<Child>) {
    if let Some(mut child) = browser.take() {
        // try_wait may already have reaped a failed startup. Never signal a
        // PID after reaping it: the kernel can have reused that process group.
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }
        // An unreaped direct child pins its PID. The child created its own
        // process group; never signal the installer's or Cage's process group.
        if let Some(pid) = i32::try_from(child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = child.kill();
        // Reaping happens only on the supervisor, never on the account actor.
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_uses_only_the_approved_installed_browser_route() {
        let session = LaunchSession {
            socket: "/run/user/42/wayland-0".into(),
            environment: vec![],
        };
        let url = "https://accounts.example/authorize?state=secret&scope=a%20b";
        let command = launch_command(&session, url);
        assert_eq!(command.get_program(), LAUNCHER);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "exec",
                "--package",
                PACKAGE,
                "--wayland-socket",
                "/run/user/42/wayland-0",
                "--open-uri",
                url
            ]
        );
        assert_eq!(
            command.get_envs().collect::<Vec<_>>(),
            [(
                std::ffi::OsStr::new("PATH"),
                Some(std::ffi::OsStr::new("/usr/bin:/bin"))
            )]
        );
    }

    #[test]
    fn only_web_authorization_urls_and_loopback_fixtures_are_accepted() {
        for value in [
            "https://accounts.example/authorize?state=secret",
            "http://127.0.0.1:4567/authorize",
            "http://[::1]:4567/authorize",
        ] {
            assert_eq!(validate_url(value), Ok(()));
        }
        for value in [
            "--allow-unsigned-runtime",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://accounts.example/",
            "https://user:password@example.org/",
            "https://example.org/#secret",
            "https://example.org/\nsecret",
        ] {
            assert_eq!(validate_url(value), Err(BrowserError::InvalidUrl));
        }
        assert_eq!(
            validate_url(&"x".repeat(MAX_URL_BYTES + 1)),
            Err(BrowserError::InvalidUrl)
        );
    }

    #[test]
    fn cage_socket_must_exist_and_belong_to_the_session() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("wayland-0");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let uid = rustix::process::geteuid().as_raw();
        assert_eq!(
            display_socket(directory.path(), std::ffi::OsStr::new("wayland-0"), uid),
            Ok(socket)
        );
        assert_eq!(
            display_socket(directory.path(), std::ffi::OsStr::new("missing"), uid),
            Err(BrowserError::DisplayUnavailable)
        );
        assert_eq!(
            display_socket(
                directory.path(),
                std::ffi::OsStr::new("wayland-0"),
                uid.wrapping_add(1)
            ),
            Err(BrowserError::DisplayUnavailable)
        );
    }

    #[test]
    fn early_exit_is_a_failure_even_with_a_success_exit_code() {
        let mut browser = Some(
            Command::new("/usr/bin/true")
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let result = startup(
            &mut browser,
            Instant::now().checked_add(OPEN_DEADLINE).unwrap(),
        );
        stop(&mut browser);
        assert_eq!(result, Err(BrowserError::LaunchFailed));
    }

    #[test]
    fn a_running_child_is_bounded_and_can_be_closed() {
        let mut browser = Some(
            Command::new("/usr/bin/sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .unwrap(),
        );
        let result = startup(
            &mut browser,
            Instant::now()
                .checked_add(Duration::from_millis(50))
                .unwrap(),
        );
        stop(&mut browser);
        assert_eq!(result, Err(BrowserError::TimedOut));
        assert!(browser.is_none());
    }

    #[test]
    fn browser_policy_keeps_the_canonical_oauth_loopback_reachable() {
        let manifest = include_str!("../../../../../apps/browser/tilo-app.toml");
        // The installed signed manifest is consumed by the launcher, not edited
        // or overridden here. Host mode already supplies --share-net.
        assert!(manifest.contains("[sandbox.net]\nmode = \"host\""));
        assert!(manifest.contains("landlock   = \"strict\""));
        assert!(manifest.contains("seccomp    = \"tilo-web-engine\""));
    }
}
