//! Session-owned on-screen keyboard lifecycle.
//!
//! Settings stores intent; the compositor owns execution because it also owns
//! the private Wayland socket carrying `input-method-v2`. The keyboard is
//! launched through the ordinary package sandbox, with exactly that socket
//! bind-mounted as its display. Disabling the preference terminates the whole
//! launcher/bwrap process group, so an old input method cannot linger after the
//! user turns it off.

use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use crate::shell::state::RoomId;

const PACKAGE_ID: &str = super::state::OSK_APP_ID;
const PACKAGE_KIND: &str = "service";
const RETRY_BACKOFF: Duration = Duration::from_secs(5);

pub fn preference_enabled() -> bool {
    settings_store::load_section("accessibility")
        .get("on_screen_keyboard")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

pub struct OskService {
    socket: PathBuf,
    accepting: Arc<AtomicBool>,
    process_group: Arc<AtomicI32>,
    running: Arc<AtomicBool>,
    desired: bool,
    stop: Option<Sender<()>>,
    last_attempt: Option<Instant>,
}

impl OskService {
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            accepting: Arc::new(AtomicBool::new(false)),
            process_group: Arc::new(AtomicI32::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            desired: false,
            stop: None,
            last_attempt: None,
        }
    }

    /// Shared with the socket callback. A disabled service rejects new
    /// connections even though the listening source stays registered.
    pub fn socket_authorization(&self) -> OskSocketAuthorization {
        OskSocketAuthorization {
            accepting: Arc::clone(&self.accepting),
            process_group: Arc::clone(&self.process_group),
        }
    }

    pub fn reload_preference(&mut self) {
        self.set_enabled(preference_enabled());
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.desired = enabled;
        self.accepting.store(enabled, Ordering::Release);
        if !enabled {
            self.process_group.store(0, Ordering::Release);
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            return;
        }
        self.ensure_running();
    }

    /// Retry a crashed/missing service with a bounded backoff. Called from the
    /// compositor tick; it never waits for the child.
    pub fn tick(&mut self) {
        if self.desired && !self.running.load(Ordering::Acquire) {
            self.ensure_running();
        }
    }

    fn ensure_running(&mut self) {
        if self.running.load(Ordering::Acquire)
            || self
                .last_attempt
                .is_some_and(|at| at.elapsed() < RETRY_BACKOFF)
        {
            return;
        }
        self.last_attempt = Some(Instant::now());

        let command = match service_launch_command(&self.socket) {
            Ok(command) => command,
            Err(error) => {
                tracing::warn!(target: "qos::osk", %error, "on-screen keyboard unavailable");
                return;
            }
        };
        let (tx, rx) = channel();
        self.stop = Some(tx);
        let running = Arc::clone(&self.running);
        let process_group = Arc::clone(&self.process_group);
        let supervisor = std::thread::Builder::new()
            .name("qos-osk-service".to_string())
            .spawn(move || {
                let mut cmd = Command::new(&command.program);
                cmd.args(&command.args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    // Launcher, bwrap and OSK share one group. The supervisor
                    // can therefore revoke the service as one unit.
                    .process_group(0);
                for (key, value) in command.env {
                    cmd.env(key, value);
                }
                let mut child = match cmd.spawn() {
                    Ok(child) => child,
                    Err(error) => {
                        tracing::warn!(target: "qos::osk", %error, "could not start on-screen keyboard");
                        return;
                    }
                };
                running.store(true, Ordering::Release);
                let pgid = child.id() as libc::pid_t;
                process_group.store(pgid, Ordering::Release);
                loop {
                    if rx.recv_timeout(Duration::from_millis(150)).is_ok() {
                        terminate_group(&mut child, pgid);
                        break;
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            if !status.success() {
                                tracing::warn!(target: "qos::osk", ?status, "on-screen keyboard exited");
                            }
                            break;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            tracing::warn!(target: "qos::osk", %error, "could not inspect on-screen keyboard");
                            break;
                        }
                    }
                }
                process_group.store(0, Ordering::Release);
                running.store(false, Ordering::Release);
            });
        if let Err(error) = supervisor {
            self.stop = None;
            self.running.store(false, Ordering::Release);
            tracing::warn!(target: "qos::osk", %error, "could not create OSK supervisor thread");
        }
    }
}

impl Drop for OskService {
    fn drop(&mut self) {
        self.accepting.store(false, Ordering::Release);
        self.process_group.store(0, Ordering::Release);
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

/// Read-only credential shared with the listening-socket callback. A random,
/// unexported path limits discovery; `SO_PEERCRED` plus the compositor-created
/// process group is the actual authorization check.
#[derive(Clone)]
pub struct OskSocketAuthorization {
    accepting: Arc<AtomicBool>,
    process_group: Arc<AtomicI32>,
}

impl OskSocketAuthorization {
    pub fn allows(&self, stream: &UnixStream) -> bool {
        if !self.accepting.load(Ordering::Acquire) {
            return false;
        }
        let expected = self.process_group.load(Ordering::Acquire);
        if expected <= 0 {
            return false;
        }
        let Some(credentials) = peer_credentials(stream) else {
            return false;
        };
        // SAFETY: `credentials.pid` came from the kernel's SO_PEERCRED
        // response and geteuid has no failure mode.
        let peer_group = unsafe { libc::getpgid(credentials.pid) };
        let expected_uid = unsafe { libc::geteuid() };
        credential_decision(
            expected,
            expected_uid,
            credentials.uid,
            peer_group,
            is_descendant_of(credentials.pid, expected),
        )
    }
}

fn peer_credentials(stream: &UnixStream) -> Option<libc::ucred> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    // `size_of::<ucred>()` is a small compile-time-fixed struct size; it can never actually
    // exceed `u32::MAX`, but a checked conversion still beats trusting that by construction.
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).ok()?;
    // SAFETY: both pointers refer to initialized storage with the advertised
    // length; `stream` remains alive throughout the call.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut credentials).cast(),
            std::ptr::from_mut(&mut length),
        )
    };
    (result == 0 && length as usize >= std::mem::size_of::<libc::ucred>()).then_some(credentials)
}

fn credential_decision(
    expected_root_and_group: libc::pid_t,
    expected_uid: libc::uid_t,
    peer_uid: libc::uid_t,
    peer_group: libc::pid_t,
    peer_is_descendant: bool,
) -> bool {
    expected_root_and_group > 0
        && peer_uid == expected_uid
        && peer_group == expected_root_and_group
        && peer_is_descendant
}

fn is_descendant_of(mut pid: libc::pid_t, ancestor: libc::pid_t) -> bool {
    for _ in 0..64 {
        if pid == ancestor {
            return true;
        }
        if pid <= 1 {
            return false;
        }
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            return false;
        };
        let Some(parent) = status.lines().find_map(|line| {
            line.strip_prefix("PPid:")
                .and_then(|value| value.trim().parse::<libc::pid_t>().ok())
        }) else {
            return false;
        };
        if parent == pid {
            return false;
        }
        pid = parent;
    }
    false
}

fn service_launch_command(socket: &Path) -> Result<crate::shell::launcher::LaunchCommand, String> {
    let app = crate::shell::launcher::read_installed_apps()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|app| app.package_id == PACKAGE_ID)
        .ok_or_else(|| format!("{PACKAGE_ID} is not installed"))?;
    let manifest = launcher::manifest::read_manifest(&app.manifest_path)
        .map_err(|e| format!("reading {}: {e}", app.manifest_path.display()))?;
    if manifest.kind != PACKAGE_KIND || manifest.entrypoint.gui_bin().is_none() {
        return Err(format!(
            "{PACKAGE_ID} is not a graphical service in its signed manifest"
        ));
    }
    let launcher = crate::shell::launcher::resolve_launcher_bin().map_err(|e| e.to_string())?;
    Ok(crate::shell::launcher::build_launch_command(
        &app,
        &launcher,
        socket,
        RoomId::nil(),
        None,
        None,
        crate::compositor::render_node::resolved(),
        None,
    ))
}

fn terminate_group(child: &mut std::process::Child, pgid: libc::pid_t) {
    // `pgid` is always the positive pid returned for the child we created with
    // `process_group(0)`, so this never overflows; `checked_neg` still keeps the kill from
    // firing on a garbage value instead of trusting the invariant blindly.
    if let Some(target) = pgid.checked_neg() {
        // SAFETY: `target` is the negation of the positive pid returned for the child we
        // created with `process_group(0)`. A negative argument targets exactly that group.
        unsafe {
            libc::kill(target, libc::SIGTERM);
        }
    }
    // A one-second deadline this close to `Instant`'s representable range is not a real
    // possibility, but falling back to "now" (skip straight to SIGKILL) beats hanging.
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if let Some(target) = pgid.checked_neg() {
        // SAFETY: same process group as above; bounded escalation prevents a
        // wedged sandbox from retaining input authority indefinitely.
        unsafe {
            libc::kill(target, libc::SIGKILL);
        }
    }
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_controller_closes_the_accept_gate() {
        let mut service = OskService::new(PathBuf::from("/not/opened"));
        service.desired = true;
        service.accepting.store(true, Ordering::Release);
        service.process_group.store(42, Ordering::Release);
        service.set_enabled(false);
        assert!(!service.accepting.load(Ordering::Acquire));
        assert_eq!(service.process_group.load(Ordering::Acquire), 0);
        assert!(!service.desired);
    }

    #[test]
    fn socket_authorization_rejects_closed_gate_and_wrong_group() {
        let service = OskService::new(PathBuf::from("/not/opened"));
        let authorization = service.socket_authorization();
        let (client, _server) = UnixStream::pair().expect("socket pair");

        authorization.accepting.store(true, Ordering::Release);
        assert!(!authorization.allows(&client), "no launched group yet");

        authorization
            .process_group
            .store(i32::MAX, Ordering::Release);
        assert!(!authorization.allows(&client), "wrong process group");

        authorization.accepting.store(false, Ordering::Release);
        assert!(!authorization.allows(&client));
    }

    #[test]
    fn wrong_group_or_same_uid_sibling_is_rejected() {
        // SAFETY: geteuid has no failure mode.
        let uid = unsafe { libc::geteuid() };
        assert!(credential_decision(4100, uid, uid, 4100, true));
        assert!(!credential_decision(4100, uid, uid, 4101, true));
        assert!(!credential_decision(4100, uid, uid, 4100, false));
        assert!(!credential_decision(
            4100,
            uid,
            uid.saturating_add(1),
            4100,
            true
        ));
    }

    #[test]
    fn child_process_group_is_dedicated() {
        // SAFETY: getpgrp has no failure mode.
        let parent_group = unsafe { libc::getpgrp() };
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "read _"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command.spawn().expect("spawn process-group probe");
        let pid = child.id() as libc::pid_t;
        // SAFETY: `pid` names the live child above.
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        assert_ne!(pid, parent_group);
        drop(child.stdin.take());
        let _ = child.wait();
    }
}
