//! [`Apply`] on a real dev image: users, systemd drop-ins, sysext, vkms seats.
//!
//! Everything here shells out to the same tools an administrator would use
//! (`useradd`, `systemctl`, `systemd-sysext`, `loginctl`, `journalctl`), so
//! what tilo-vmd does can be reproduced and inspected by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tilo_vm_protocol::layer_hash::walk;
use tilo_vm_protocol::{BootChange, BootConfirmResult, ControlResult, InputAction, OutputSpec};

use crate::apply::{Apply, GREETER_UNIT, Session, SessionProcess};
use crate::boot::Boot;
use crate::error::DaemonError;
use crate::fsutil::{symlink, write_atomic};
use crate::input;
use crate::slots::{DeviceSession, Login};

const UNIT_DIR: &str = "/etc/systemd/system";
/// Drop-ins that bind a slot's session view, a mount that exists only until
/// the machine shuts down, live here and go with it. Kept in `/etc`, they
/// outlived the view: after a reboot, or on an image without tilo-vmd to
/// mount it again, the session bound an empty directory over `/usr` and never
/// started. `tilo-vm-restore` writes them again with the view at boot.
const RUNTIME_UNIT_DIR: &str = "/run/systemd/system";

fn dropin_dirs(file: &str) -> [&'static str; 2] {
    if file == crate::apply::LAYERS_DROPIN {
        [RUNTIME_UNIT_DIR, UNIT_DIR]
    } else {
        [UNIT_DIR, RUNTIME_UNIT_DIR]
    }
}

fn remove_dropin_in(dir: &str, unit: &str, file: &str) -> Result<(), DaemonError> {
    let dir = Path::new(dir).join(format!("{unit}.d"));
    match fs::remove_file(dir.join(file)) {
        Ok(()) => {
            // Only succeeds when empty, which is the point.
            let _ = fs::remove_dir(&dir);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(DaemonError::io(format!("remove drop-in for {unit}"))(e)),
    }
}
const SYSEXT_DIR: &str = "/var/lib/extensions";
/// Where tilo-vmd leaves notices for the owner's desktop; the shell reads
/// `held-layers.json` from here.
const NOTICE_DIR: &str = "/run/tilo-vm";
/// Where `systemd-sysext` finds extension trees, `SYSEXT_DIR` among them.
const SYSEXT_SEARCH_DIRS: &[&str] = &["/etc/extensions", "/run/extensions", SYSEXT_DIR];
/// A registry larger than this is not one `tilo-install` wrote.
const USER_REGISTRY_MAX_BYTES: u64 = 8 * 1024 * 1024;
const VKMS_CONFIGFS: &str = "/sys/kernel/config/vkms";
/// Not in production images; the dev image installs it.
const TILO_CONTROL: &str = "/usr/bin/tilo-control";
/// The groups `tilo-firstboot-user` gives a login, less `wheel`: `tilo-identity`
/// reads the machine's device key, which provisioning an identity needs, and
/// `video` opens `/dev/dri`.
const SLOT_USER_GROUPS: &str = "tilo-identity,video";

pub struct SystemdApply {
    layers_dir: PathBuf,
    views_dir: PathBuf,
    boot: Boot,
    cohort_view: Option<(String, Vec<PathBuf>, PathBuf)>,
}

impl SystemdApply {
    fn cohort_environment(
        &mut self,
        session: &Session,
        uid: u32,
    ) -> Result<Vec<(String, String)>, DaemonError> {
        let pid = self.session_pid(session).ok_or_else(|| {
            DaemonError::InvalidParams("the admitted session has no live process".into())
        })?;
        cohort_process_environment(pid, uid)
    }

    pub fn new(root: &Path) -> Self {
        Self {
            layers_dir: root.join("layers"),
            views_dir: root.join("views"),
            boot: Boot::new(root),
            cohort_view: None,
        }
    }
}

/// A board dev image runs its account's session without a greeter
/// (os/dev/install-board-layer.sh): it installs this marker and enables one
/// instance of [`DEVICE_SESSION_TEMPLATE`], named after the account.
const DEVICE_SESSION_MARKER: &str = "/usr/share/tilo/board-headless-session";
const DEVICE_SESSION_TEMPLATE: &str = "tilo-board-headless-session@";
/// The unit `tilo-seat-session` runs the compositor in while a display is
/// connected, a PAM login on seat0. The headless compositor runs in the
/// account's user manager, which gets the layers anyway.
const DEVICE_SESSION_UNITS: &[&str] = &["tilo-board-drm-session.service"];

/// The board session's unit among `systemctl list-units --plain
/// --no-legend` lines: the active instance, else the first one listed.
fn device_session_unit(listing: &str) -> Option<String> {
    let units: Vec<(&str, &str)> = listing
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let unit = words.next()?;
            let active = words.nth(1).unwrap_or_default();
            let account = unit
                .strip_prefix(DEVICE_SESSION_TEMPLATE)?
                .strip_suffix(".service")?;
            (!account.is_empty()).then_some((unit, active))
        })
        .collect();
    units
        .iter()
        .find(|(_, active)| *active == "active")
        .or_else(|| units.first())
        .map(|(unit, _)| (*unit).to_owned())
}

/// One logind session, as `loginctl show-session` describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LogindSession {
    id: String,
    name: String,
    seat: String,
    class: String,
    remote: bool,
    state: String,
    leader: u32,
    scope: String,
}

/// `loginctl show-session <id>` output (`Key=value` lines).
fn parse_logind_session(id: &str, text: &str) -> LogindSession {
    let mut session = LogindSession {
        id: id.to_owned(),
        ..LogindSession::default()
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().to_owned();
        match key {
            "Name" => session.name = value,
            "Seat" => session.seat = value,
            "Class" => session.class = value,
            "Remote" => session.remote = value == "yes",
            "State" => session.state = value,
            "Leader" => session.leader = value.parse().unwrap_or(0),
            "Scope" => session.scope = value,
            _ => {}
        }
    }
    session
}

/// The owner's login among `sessions`: a local user session on seat0 that
/// greetd (main process `greetd`) started; its session worker, the leader,
/// is greetd's child. `parent` gives a process's parent. The greeter's own
/// session is of class `greeter`, and a tilo-vm slot's session is started by
/// systemd, not greetd.
fn greetd_login(
    sessions: &[LogindSession],
    greetd: u32,
    parent: impl Fn(u32) -> Option<u32>,
) -> Option<&LogindSession> {
    sessions.iter().find(|s| {
        s.seat == "seat0"
            && s.class == "user"
            && !s.remote
            && matches!(s.state.as_str(), "active" | "online")
            && s.leader > 0
            && greetd > 0
            && parent(s.leader) == Some(greetd)
            && s.scope.starts_with("session-")
    })
}

/// The process name `tilo-session` runs under (its script's file name).
const LOGIN_WRAPPER: &str = "tilo-session";
/// The compositor's process name.
const COMPOSITOR: &str = "shell";

fn parent_of(pid: u32) -> Option<u32> {
    proc_field(pid, "PPid:")?.parse().ok()
}

fn comm(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|c| c.trim().to_owned())
}

/// The processes in the cgroup `pid` belongs to (a login's session scope).
fn cgroup_members(pid: u32) -> Vec<u32> {
    let Some(path) = fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("0::").map(str::to_owned))
        })
    else {
        return Vec::new();
    };
    fs::read_to_string(format!("/sys/fs/cgroup{path}/cgroup.procs"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect()
}

/// The running compositor `wrapper` (a `tilo-session`) started.
fn compositor_of(wrapper: u32) -> Option<u32> {
    fs::read_to_string(format!("/proc/{wrapper}/task/{wrapper}/children"))
        .ok()?
        .split_whitespace()
        .filter_map(|c| c.parse::<u32>().ok())
        .find(|c| comm(*c).as_deref() == Some(COMPOSITOR) && !zombie(*c))
}

fn zombie(pid: u32) -> bool {
    proc_field(pid, "State:").is_none_or(|state| state.starts_with('Z'))
}

/// The owner's Tilo login through greetd on seat0, if someone is logged in.
fn login_session() -> Option<DeviceSession> {
    let greetd = Command::new("systemctl")
        .args(["show", "--property=MainPID", "--value", GREETER_UNIT])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u32>()
                .ok()
        })
        .filter(|pid| *pid > 0)?;
    let listing = Command::new("loginctl")
        .args(["list-sessions", "--no-legend"])
        .output()
        .ok()?;
    let sessions: Vec<LogindSession> = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|id| {
            let shown = Command::new("loginctl")
                .args(["show-session", id])
                .output()
                .ok()?;
            Some(parse_logind_session(
                id,
                &String::from_utf8_lossy(&shown.stdout),
            ))
        })
        .collect();
    let login = greetd_login(&sessions, greetd, parent_of)?;
    let uid = uid_of(&login.name)?;
    // A Tilo login runs tilo-session as its user; another desktop is not
    // the device slot.
    let wrapper = cgroup_members(login.leader).into_iter().find(|pid| {
        comm(*pid).as_deref() == Some(LOGIN_WRAPPER)
            && proc_field(*pid, "Uid:").and_then(|u| u.split_whitespace().next().map(str::to_owned))
                == Some(uid.to_string())
    })?;
    Some(DeviceSession {
        unit: login.scope.clone(),
        account: login.name.clone(),
        units: Vec::new(),
        control_socket: device_control_socket(uid),
        login: Some(Login {
            session: Some(login.id.clone()),
            wrapper: Some(wrapper),
        }),
    })
}

/// The compositor's control socket in the board session
/// (`TILO_TEST_CONTROL`, set by `tilo-seat-session`) and in a dev image's
/// greetd login (set by `tilo-session`).
fn device_control_socket(uid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}/tilo-session/test-control.sock"))
}

/// A component update preserves the operator's service activation state.
/// Validate the installed unit first: try-restart alone can be a successful
/// no-op for an absent unit on some systemd versions.
fn try_restart_unit_with(
    user: Option<&str>,
    unit: &str,
    mut execute: impl FnMut(&str, &mut Command) -> Result<String, DaemonError>,
) -> Result<(), DaemonError> {
    if user.is_some() {
        execute(
            "reload component user manager",
            &mut manager_command(user, &["daemon-reload"]),
        )?;
    }
    validate_unit_with(user, unit, &mut execute)?;
    execute(
        &format!("systemctl try-restart {unit}"),
        &mut manager_command(user, &["try-restart", unit]),
    )
    .map(drop)
}

/// The same installed-unit check for immediate and deferred activation.
fn validate_unit_with(
    user: Option<&str>,
    unit: &str,
    execute: &mut impl FnMut(&str, &mut Command) -> Result<String, DaemonError>,
) -> Result<(), DaemonError> {
    let loaded = execute(
        "validate component unit",
        &mut manager_command(user, &["show", "--property=LoadState", "--value", unit]),
    )?;
    if !matches!(loaded.trim(), "loaded" | "merged") {
        return Err(DaemonError::Apply {
            step: format!("validate component unit {unit}"),
            detail: format!("unit is not loadable ({})", loaded.trim()),
        });
    }
    Ok(())
}

fn manager_command(user: Option<&str>, args: &[&str]) -> Command {
    let mut command = Command::new("systemctl");
    if let Some(user) = user {
        command.arg(format!("--machine={user}@")).arg("--user");
    }
    command.args(args);
    command
}

fn run(step: &str, command: &mut Command) -> Result<String, DaemonError> {
    let output = command.output().map_err(|e| DaemonError::Apply {
        step: step.to_owned(),
        detail: e.to_string(),
    })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(DaemonError::Apply {
            step: step.to_owned(),
            detail: format!(
                "{}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        })
    }
}

fn systemctl(verb: &str, unit: &str) -> Result<(), DaemonError> {
    run(
        &format!("systemctl {verb} {unit}"),
        Command::new("systemctl").args([verb, unit]),
    )
    .map(drop)
}

/// How long the compositor may take to paint and encode a snapshot after
/// acknowledging it: the reply comes before the frame that writes the file.
const SNAPSHOT_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// The PNG at `path`, once it is there in full.
fn await_png(path: &Path, timeout: std::time::Duration) -> Result<Vec<u8>, DaemonError> {
    let started = std::time::Instant::now();
    loop {
        match fs::read(path) {
            Ok(bytes) if png_complete(&bytes) => return Ok(bytes),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(DaemonError::io("read snapshot")(error)),
        }
        if started.elapsed() >= timeout {
            return Err(DaemonError::Apply {
                step: "read snapshot".into(),
                detail: format!(
                    "the compositor acknowledged the snapshot but wrote no complete PNG to {} \
                     within {}s; its journal says why",
                    path.display(),
                    timeout.as_secs()
                ),
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Whether `bytes` is a whole PNG: the signature, and the `IEND` chunk last.
fn png_complete(bytes: &[u8]) -> bool {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    const IEND: &[u8] = b"\0\0\0\0IEND\xae\x42\x60\x82";
    bytes.starts_with(SIGNATURE)
        && bytes
            .get(SIGNATURE.len()..)
            .is_some_and(|rest| rest.ends_with(IEND))
}

fn uid_of(user: &str) -> Option<u32> {
    let output = Command::new("getent")
        .args(["passwd", user])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&output.stdout).into_owned();
    line.split(':').nth(2)?.trim().parse().ok()
}

impl Apply for SystemdApply {
    fn device_action_owner(&mut self, session: &Session) -> bool {
        let Some(home) =
            home_of(&session.user).and_then(|home| PathBuf::from(home).canonicalize().ok())
        else {
            return false;
        };
        if !crate::disk_unlock::account_is_admin(&session.user) {
            return false;
        }
        let config = self
            .session_environment(session)
            .into_iter()
            .find(|(name, _)| name == "XDG_CONFIG_HOME")
            .map_or_else(|| home.join(".config"), |(_, value)| PathBuf::from(value));
        let Ok(identity) = config.join("tilo/identity").canonicalize() else {
            return false;
        };
        identity.starts_with(&home)
            && tilo_identity_host::provisioning::active_identity_owns_device(
                &identity,
                &tilo_user_identity::IdentityPaths::under(Path::new("/var/lib/tilo")),
            )
            .unwrap_or(false)
    }
    fn enroll_device_unlock(
        &mut self,
        uid: u32,
        request: &tilo_vm_protocol::DeviceUnlockEnrol,
    ) -> Result<tilo_vm_protocol::DeviceUnlockEnrolResult, DaemonError> {
        crate::disk_unlock::enroll(uid, request)
    }
    fn session_layer_dropin(
        &mut self,
        slot: &str,
        generation: u64,
        layers: &[PathBuf],
    ) -> Result<String, DaemonError> {
        self.cohort_view = None;
        let view = crate::session_view::prepare(&self.views_dir, slot, layers)?;
        Ok(format!(
            "# Written by tilo-vmd: slot {slot}, generation {generation}. Do not edit.\n[Service]\nExtensionDirectories=\nBindReadOnlyPaths={}/usr:/usr {}/opt:/opt\n",
            view.display(),
            view.display()
        ))
    }
    fn remove_session_view(&mut self, slot: &str) -> Result<(), DaemonError> {
        crate::session_view::remove(&self.views_dir, slot)
    }
    fn boot_kernel(
        &mut self,
        layer: Option<&Path>,
    ) -> Result<(BootChange, Vec<String>), DaemonError> {
        match layer {
            Some(layer) => self.boot.install(layer),
            None => self.boot.restore_image().map(|change| (change, Vec::new())),
        }
    }

    fn confirm_boot(
        &mut self,
        expect_version: Option<&str>,
    ) -> Result<BootConfirmResult, DaemonError> {
        self.boot.confirm(expect_version)
    }

    fn hold_awake(&mut self, lease: &Path) -> Result<(), DaemonError> {
        let unit = crate::awake::AWAKE_UNIT;
        if self.unit_state(unit) == "active" {
            // It rereads the lease and takes the new deadline itself.
            return Ok(());
        }
        let _ = Command::new("systemctl")
            .args(["reset-failed", unit])
            .output();
        let exe = std::env::current_exe().map_err(DaemonError::io("find the tilo-vmd binary"))?;
        let mut command = Command::new("systemd-run");
        command
            .arg(format!("--unit={unit}"))
            .args(["--collect", "--quiet"])
            .arg("--description=tilo-vm keeps this machine awake while someone works on it")
            .arg(exe)
            .args(["awake-hold", "--lease"])
            .arg(lease);
        match run(&format!("systemd-run --unit {unit}"), &mut command) {
            Ok(_) => Ok(()),
            // Another connection started it meanwhile.
            Err(_) if self.unit_state(unit) == "active" => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn release_awake(&mut self) -> Result<(), DaemonError> {
        let unit = crate::awake::AWAKE_UNIT;
        if self.unit_state(unit) == "active" {
            systemctl("stop", unit)?;
        }
        Ok(())
    }

    fn ensure_user(&mut self, user: &str) -> Result<u32, DaemonError> {
        if let Some(uid) = uid_of(user) {
            run(
                "usermod",
                Command::new("usermod").args(["--append", "--groups", SLOT_USER_GROUPS, user]),
            )?;
            return Ok(uid);
        }
        // A regular (non-system) user, like the production login it stands in for.
        run(
            "useradd",
            Command::new("useradd").args([
                "--create-home",
                "--user-group",
                "--comment",
                "Tilo dev slot",
                "--groups",
                SLOT_USER_GROUPS,
                user,
            ]),
        )?;
        uid_of(user).ok_or_else(|| DaemonError::Apply {
            step: "useradd".into(),
            detail: format!("{user} has no uid after creation"),
        })
    }

    fn account_uid(&mut self, user: &str) -> Result<u32, DaemonError> {
        uid_of(user).ok_or_else(|| DaemonError::Apply {
            step: "getent passwd".into(),
            detail: format!("no account {user}"),
        })
    }

    fn device_session(&mut self) -> Option<DeviceSession> {
        if !Path::new(DEVICE_SESSION_MARKER).exists() {
            return login_session();
        }
        let listing = Command::new("systemctl")
            .args([
                "list-units",
                "--all",
                "--plain",
                "--no-legend",
                "--type=service",
            ])
            .arg(format!("{DEVICE_SESSION_TEMPLATE}*.service"))
            .output()
            .ok()?;
        let unit = device_session_unit(&String::from_utf8_lossy(&listing.stdout))?;
        let account = unit
            .strip_prefix(DEVICE_SESSION_TEMPLATE)?
            .strip_suffix(".service")?
            .to_owned();
        let uid = uid_of(&account)?;
        Some(DeviceSession {
            unit,
            account,
            units: DEVICE_SESSION_UNITS
                .iter()
                .map(|u| (*u).to_owned())
                .collect(),
            control_socket: device_control_socket(uid),
            login: None,
        })
    }

    fn slot_identity_environment(&mut self, user: &str) -> Result<String, DaemonError> {
        let home = home_of(user).ok_or_else(|| DaemonError::Apply {
            step: "resolve slot identity home".into(),
            detail: format!("no home directory for {user}"),
        })?;
        crate::apply::slot_identity_environment(Path::new(&home))
    }

    fn remove_user(&mut self, user: &str) -> Result<(), DaemonError> {
        if uid_of(user).is_none() {
            return Ok(());
        }
        run("userdel", Command::new("userdel").args(["--remove", user])).map(drop)
    }

    fn write_dropin(&mut self, unit: &str, file: &str, contents: &str) -> Result<(), DaemonError> {
        let [dir, other] = dropin_dirs(file);
        write_atomic(
            &Path::new(dir).join(format!("{unit}.d")).join(file),
            contents.as_bytes(),
        )?;
        // A copy an older tilo-vmd wrote to the other place would win or linger.
        remove_dropin_in(other, unit, file)
    }

    fn remove_dropin(&mut self, unit: &str, file: &str) -> Result<(), DaemonError> {
        dropin_dirs(file)
            .iter()
            .try_for_each(|dir| remove_dropin_in(dir, unit, file))
    }

    fn reload(&mut self) -> Result<(), DaemonError> {
        run(
            "systemctl daemon-reload",
            Command::new("systemctl").arg("daemon-reload"),
        )
        .map(drop)
    }

    fn start_unit(&mut self, unit: &str) -> Result<(), DaemonError> {
        systemctl("start", unit)
    }

    fn stop_unit(&mut self, unit: &str) -> Result<(), DaemonError> {
        systemctl("stop", unit)
    }

    fn restart_unit(&mut self, unit: &str) -> Result<(), DaemonError> {
        systemctl("restart", unit)
    }

    fn validate_unit(&mut self, unit: &str) -> Result<(), DaemonError> {
        validate_unit_with(None, unit, &mut run)
    }
    fn try_restart_unit(&mut self, unit: &str) -> Result<(), DaemonError> {
        try_restart_unit_with(None, unit, run)
    }

    fn restart_desktop(&mut self, session: &Session) -> Result<(), DaemonError> {
        let step = format!("restart {}'s desktop", session.user);
        let fail = |detail: String| DaemonError::Apply {
            step: step.clone(),
            detail,
        };
        let wrapper = session
            .login
            .as_ref()
            .and_then(|login| login.wrapper)
            .filter(|pid| comm(*pid).as_deref() == Some(LOGIN_WRAPPER))
            .ok_or_else(|| fail("the login's tilo-session is gone: the owner logged out".into()))?;
        let old = compositor_of(wrapper);
        run(
            &step,
            Command::new("kill").args(["-USR1", &wrapper.to_string()]),
        )?;
        let Some(old) = old else {
            return Ok(());
        };
        // tilo-session sends the compositor SIGTERM and waits for it.
        let started = std::time::Instant::now();
        while Path::new(&format!("/proc/{old}")).exists() && !zombie(old) {
            if started.elapsed() >= DESKTOP_EXIT_TIMEOUT {
                return Err(fail(format!(
                    "the compositor (pid {old}) did not exit within {}s of SIGTERM",
                    DESKTOP_EXIT_TIMEOUT.as_secs()
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Ok(())
    }

    fn restart_user_unit(&mut self, user: &str, unit: &str) -> Result<(), DaemonError> {
        try_restart_unit_with(Some(user), unit, run)
    }

    fn unit_state(&mut self, unit: &str) -> String {
        // is-active exits non-zero for anything but "active"; stdout is the answer.
        Command::new("systemctl")
            .args(["is-active", unit])
            .output()
            .map_or_else(
                |_| "unknown".into(),
                |o| String::from_utf8_lossy(&o.stdout).trim().to_owned(),
            )
    }

    fn label_layer(&mut self, layer: &Path) -> Result<(), DaemonError> {
        let Some(contexts) = file_contexts() else {
            return Ok(());
        };
        // Label as if the layer were mounted at `/`: usr/bin/* -> bin_t, and
        // so on. Unlabelled var_lib_t files cannot be executed through the slot.
        let mut command = Command::new("setfiles");
        command.arg("-r").arg(layer).arg(&contexts);
        for top in ["usr", "opt"] {
            if layer.join(top).is_dir() {
                command.arg(layer.join(top));
            }
        }
        run("setfiles", &mut command).map(drop)
    }

    // Provisional: systemd-sysext accepting symlinks to directories in
    // /var/lib/extensions (named after the image, i.e. the content hash).
    fn link_sysext(&mut self, layers: &[PathBuf]) -> Result<bool, DaemonError> {
        let dir = Path::new(SYSEXT_DIR);
        let mut changed = false;
        fs::create_dir_all(dir).map_err(DaemonError::io("create /var/lib/extensions"))?;
        // Drop our stale links, never anything another tool put there.
        for entry in fs::read_dir(dir)
            .map_err(DaemonError::io("read /var/lib/extensions"))?
            .flatten()
        {
            let link = entry.path();
            if let Ok(target) = fs::read_link(&link)
                && target.starts_with(&self.layers_dir)
                && !layers.contains(&target)
            {
                fs::remove_file(&link)
                    .map_err(DaemonError::io(format!("unlink {}", link.display())))?;
                changed = true;
            }
        }
        for layer in layers {
            let Some(name) = layer.file_name() else {
                continue;
            };
            let link = dir.join(name);
            if fs::read_link(&link).ok().as_deref() != Some(layer.as_path()) {
                let _ = fs::remove_file(&link);
                symlink(layer, &link)
                    .map_err(DaemonError::io(format!("link {}", link.display())))?;
                changed = true;
            }
        }
        Ok(changed)
    }

    fn sysext_refresh(&mut self) -> Result<(), DaemonError> {
        run(
            "systemd-sysext refresh",
            Command::new("systemd-sysext").arg("refresh"),
        )?;
        // A layer may bring a system account (a sysusers.d entry, as the
        // relay's does): create it as the image build would.
        run("systemd-sysusers", &mut Command::new("systemd-sysusers")).map(drop)
    }

    fn publish_notice(&mut self, name: &str, contents: Option<&str>) -> Result<(), DaemonError> {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = Path::new(NOTICE_DIR);
        let path = dir.join(name);
        let Some(text) = contents else {
            return match fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(DaemonError::io(format!("remove {}", path.display()))(e))
                }
                _ => Ok(()),
            };
        };
        fs::create_dir_all(dir).map_err(DaemonError::io(format!("create {NOTICE_DIR}")))?;
        crate::fsutil::write_atomic(&path, text.as_bytes())?;
        // The owner's desktop reads it as its own user.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .map_err(DaemonError::io(format!("make {} readable", path.display())))
    }

    fn allocate_seat(&mut self, slot: &str, connectors: u32) -> Result<String, DaemonError> {
        // The configfs device, the logind seat and the uinput `phys` tag share
        // one name; the shipped 72-vkms-seat.rules maps the card to the seat.
        let seat = format!("seat-{slot}");
        let device = Path::new(VKMS_CONFIGFS).join(&seat);
        if !device.exists() {
            run(
                "modprobe vkms",
                Command::new("modprobe").args(["vkms", "create_default_dev=0"]),
            )?;
            vkms_create(&device, connectors)
                .map_err(DaemonError::io(format!("create vkms device {seat}")))?;
            run("udevadm settle", Command::new("udevadm").arg("settle"))?;
        }
        if vkms_card(&seat).is_none() {
            return Err(DaemonError::Apply {
                step: "allocate vkms seat".into(),
                detail: format!("no DRM card appeared under /sys/devices/faux/{seat}"),
            });
        }
        Ok(seat)
    }

    fn apply_display(
        &mut self,
        _slot: &str,
        user: &str,
        outputs: &[OutputSpec],
    ) -> Result<String, DaemonError> {
        let uid = uid_of(user).ok_or_else(|| DaemonError::Apply {
            step: "tilo-vmd display-apply".into(),
            detail: format!("no user {user}"),
        })?;
        let home = home_of(user).unwrap_or_else(|| format!("/home/{user}"));
        let exe = std::env::current_exe().map_err(DaemonError::io("find the tilo-vmd binary"))?;
        // The session's own environment: the settings file under HOME, the
        // compositor's display-state.json under XDG_RUNTIME_DIR.
        run(
            "tilo-vmd display-apply",
            Command::new("runuser")
                .args(["-u", user, "--", "env"])
                .arg(format!("HOME={home}"))
                .arg(format!("USER={user}"))
                .arg(format!("XDG_RUNTIME_DIR=/run/user/{uid}"))
                .arg(exe)
                .arg("display-apply")
                .arg("--outputs")
                .arg(tilo_vm_protocol::display::format_outputs(outputs)),
        )
        .map(|out| out.trim().to_owned())
    }

    fn reset_desktop_state(&mut self, user: &str) -> Result<(), DaemonError> {
        let home = home_of(user).unwrap_or_else(|| format!("/home/{user}"));
        let tilo = Path::new(&home).join(".config/tilo");
        for name in DESKTOP_STATE {
            let path = tilo.join(name);
            let removed = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match removed {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(DaemonError::io(format!("remove {}", path.display()))(error));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn release_seat(&mut self, _slot: &str, seat: &str) -> Result<(), DaemonError> {
        let device = Path::new(VKMS_CONFIGFS).join(seat);
        if device.exists() {
            vkms_remove(&device).map_err(DaemonError::io(format!("remove vkms device {seat}")))?;
        }
        Ok(())
    }

    fn journal(&mut self, matches: &[String], lines: u32) -> Result<String, DaemonError> {
        let mut command = Command::new("journalctl");
        command.args([
            "--no-pager",
            "--output=short-iso",
            "--lines",
            &lines.to_string(),
        ]);
        for (index, term) in matches.iter().enumerate() {
            if index > 0 {
                command.arg("+");
            }
            command.arg(term);
        }
        run("journalctl", &mut command)
    }

    fn control(
        &mut self,
        session: &Session,
        args: &[String],
    ) -> Result<ControlResult, DaemonError> {
        if !Path::new(TILO_CONTROL).exists() {
            return Err(DaemonError::ControlMissing(TILO_CONTROL.into()));
        }
        let output = Command::new("runuser")
            .args(["-u", &session.user, "--", "env"])
            .arg(format!(
                "TILO_TEST_CONTROL={}",
                session.control_socket.display()
            ))
            .arg(TILO_CONTROL)
            .args(args)
            .output()
            .map_err(|e| DaemonError::Apply {
                step: "tilo-control".into(),
                detail: e.to_string(),
            })?;
        Ok(ControlResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn private_control(
        &mut self,
        session: &Session,
        args: &[String],
        input: &str,
    ) -> Result<ControlResult, DaemonError> {
        use std::io::Write as _;
        use std::process::Stdio;
        let pid = self
            .session_pid(session)
            .ok_or_else(|| DaemonError::InvalidParams("slot session is not running".into()))?;
        // Use the slot's verified control binary, not the older image's client.
        let mut child = Command::new("nsenter")
            .args([
                "--mount",
                "--target",
                &pid.to_string(),
                "--",
                "runuser",
                "-u",
                &session.user,
                "--",
                "env",
            ])
            .arg(format!(
                "TILO_TEST_CONTROL={}",
                session.control_socket.display()
            ))
            .arg(TILO_CONTROL)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| {
                DaemonError::InvalidParams("could not start private control client".into())
            })?;
        let sent = child
            .stdin
            .take()
            .ok_or_else(|| {
                DaemonError::InvalidParams("private control stdin is unavailable".into())
            })?
            .write_all(input.as_bytes());
        if sent.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(DaemonError::InvalidParams(
                "could not send private control arguments".into(),
            ));
        }
        let output = child
            .wait_with_output()
            .map_err(|_| DaemonError::InvalidParams("private control client failed".into()))?;
        if !output.status.success() {
            return Err(DaemonError::InvalidParams(
                "private action was refused; inspect its declared parameters and custody state"
                    .into(),
            ));
        }
        Ok(ControlResult {
            exit_code: 0,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::new(),
        })
    }

    fn wait_session_ready(&mut self, session: &Session) -> Result<(), String> {
        // Restart=always turns a failing start into active/activating flapping,
        // so the unit has to stay active under one main process for a while.
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);
        if let Some(login) = &session.login {
            return login
                .wrapper
                .map_or(Ok(()), |wrapper| wait_desktop(wrapper, SETTLE));
        }
        // A board session first waits for its user manager, the network and
        // the clock (tilo-board-session-prepare).
        let timeout = std::time::Duration::from_secs(if session.stand_in { 20 } else { 120 });
        let unit = session.unit.clone();
        let started = std::time::Instant::now();
        let mut stable: Option<(std::time::Instant, String)> = None;
        loop {
            let state = self.unit_state(&unit);
            let main = Command::new("systemctl")
                .args(["show", "--property=MainPID", "--value", &unit])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_default();
            if state == "active" && main != "0" && !main.is_empty() {
                match &stable {
                    Some((since, pid)) if *pid == main => {
                        if since.elapsed() >= SETTLE {
                            return Ok(());
                        }
                    }
                    _ => stable = Some((std::time::Instant::now(), main)),
                }
            } else {
                stable = None;
            }
            if started.elapsed() >= timeout {
                return Err(state);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    fn session_pid(&mut self, session: &Session) -> Option<u32> {
        // A login: its compositor, whose environment has the session's
        // variables, else the tilo-session around it.
        if let Some(login) = &session.login {
            let wrapper = login.wrapper?;
            return compositor_of(wrapper)
                .or_else(|| (comm(wrapper).as_deref() == Some(LOGIN_WRAPPER)).then_some(wrapper));
        }
        let output = Command::new("systemctl")
            .args(["show", "--property=MainPID", "--value", &session.unit])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .ok()
            .filter(|p| *p > 0)
    }

    fn session_environment(&mut self, session: &Session) -> Vec<(String, String)> {
        self.session_pid(session)
            .and_then(|pid| fs::read(format!("/proc/{pid}/environ")).ok())
            .map(|environ| {
                let mut env = crate::exec::session_env(&environ);
                crate::exec::add_published_wayland_display(&mut env);
                env
            })
            .unwrap_or_default()
    }

    fn session_process(&mut self, session: &Session) -> Option<SessionProcess> {
        let main = self.session_pid(session)?;
        // Type=simple returns once forked; wait for the switch to the slot user.
        for _ in 0..20 {
            if proc_field(main, "Uid:")
                .is_some_and(|uid| uid.split_whitespace().next() != Some("0"))
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // Prefer the compositor itself, tilo-session's child.
        let children =
            fs::read_to_string(format!("/proc/{main}/task/{main}/children")).unwrap_or_default();
        let pid = children
            .split_whitespace()
            .filter_map(|c| c.parse::<u32>().ok())
            .find(|c| {
                fs::read_to_string(format!("/proc/{c}/comm"))
                    .is_ok_and(|comm| comm.trim() == "shell")
            })
            .unwrap_or(main);
        let hex = |field: &str| {
            proc_field(pid, field).and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
        };
        let context = fs::read_to_string(format!("/proc/{pid}/attr/current"))
            .ok()
            .map(|c| c.trim_end_matches(['\0', '\n']).to_owned())
            .filter(|c| !c.is_empty());
        Some(SessionProcess {
            cap_amb: hex("CapAmb:")?,
            cap_eff: hex("CapEff:")?,
            context,
        })
    }

    fn disk_space(&mut self, path: &Path) -> Option<(u64, u64)> {
        let output = Command::new("df")
            .args(["--block-size=1", "--output=avail,size"])
            .arg(path)
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let mut numbers = text
            .lines()
            .nth(1)?
            .split_whitespace()
            .map(|n| n.parse::<u64>().ok());
        Some((numbers.next()??, numbers.next()??))
    }

    fn shot(&mut self, session: &Session) -> Result<Vec<u8>, DaemonError> {
        // Beside the control socket: a directory the compositor writes to.
        let path = session
            .control_socket
            .parent()
            .unwrap_or_else(|| Path::new("/run"))
            .join(format!("shot-{}.png", uuid::Uuid::new_v4().simple()));
        let result = self.control(session, &["snapshot".into(), path.display().to_string()])?;
        if result.exit_code != 0 {
            return Err(DaemonError::Apply {
                step: "tilo-control snapshot".into(),
                detail: result.stderr,
            });
        }
        let bytes = await_png(&path, SNAPSHOT_WRITE_TIMEOUT);
        let _ = fs::remove_file(&path);
        bytes
    }

    fn input(
        &mut self,
        slot: &str,
        seat: &str,
        geometry: Option<input::Geometry>,
        actions: &[InputAction],
    ) -> Result<u64, DaemonError> {
        let socket = input::socket_path(slot);
        let reply = match input::send(&socket, actions) {
            Ok(reply) if reply.seat == seat && reply.geometry == geometry => reply,
            // Not running yet, or its devices belong to the seat the slot had
            // before a seat0 claim or release: (re)create them.
            _ => {
                start_input_server(slot, seat, geometry, &socket)?;
                input::send(&socket, actions).map_err(|e| DaemonError::Apply {
                    step: format!("send input to {}", input::unit_name(slot)),
                    detail: e.to_string(),
                })?
            }
        };
        match reply.error {
            Some(detail) => Err(DaemonError::Apply {
                step: "input".into(),
                detail,
            }),
            None => Ok(reply.events),
        }
    }

    fn stop_input(&mut self, slot: &str) -> Result<(), DaemonError> {
        // A unit that is not loaded is already stopped.
        let _ = Command::new("systemctl")
            .args(["stop", &input::unit_name(slot)])
            .output();
        Ok(())
    }

    fn install_app(&mut self, session: &Session, tapp: &Path) -> Result<String, DaemonError> {
        install_app_in_session(self, session, tapp, false)
    }

    fn prepare_cohort_selection(
        &mut self,
        session: &Session,
        layers: &[PathBuf],
    ) -> Result<(), DaemonError> {
        // Another RPC process may have replaced this slot's mounted view since
        // our last request. Never reuse an earlier connection's cached identity.
        self.cohort_view = None;
        let view = crate::session_view::prepare(&self.views_dir, &session.slot, layers)?;
        self.cohort_view = Some((session.slot.clone(), layers.to_vec(), view));
        Ok(())
    }

    fn cohort_selected_apps(
        &mut self,
        session: &Session,
        layers: &[PathBuf],
    ) -> Result<crate::SelectedApps, DaemonError> {
        let view = self
            .cohort_view
            .as_ref()
            .filter(|(slot, current, _)| slot == &session.slot && current == layers)
            .map(|(_, _, view)| view.clone())
            .ok_or_else(|| DaemonError::InvalidParams("no current cohort view".into()))?;
        let uid = self.account_uid(&session.user)?;
        let home = home_of(&session.user).ok_or_else(|| {
            DaemonError::InvalidParams("the admitted session account has no home".into())
        })?;
        let environment = self.cohort_environment(session, uid)?;
        let mut command = cohort_account_command(session, uid, &home, &view, &environment);
        command.args(["list", "--json", "--runtime-paths"]);
        let text = run("read authoritative cohort selections", &mut command)?;
        if text.len() > 8 * 1024 * 1024 {
            return Err(DaemonError::InvalidParams(
                "cohort selection metadata exceeds bound".into(),
            ));
        }
        let rows: crate::SelectedApps = serde_json::from_str(&text).map_err(|error| {
            DaemonError::InvalidParams(format!("cohort selection metadata: {error}"))
        })?;
        if rows.apps.len() > 512 || rows.runtime_paths.len() > 512 {
            return Err(DaemonError::InvalidParams(
                "cohort selection count exceeds bound".into(),
            ));
        }
        Ok(rows)
    }
    fn cohort_runtime_view(
        &mut self,
        session: &Session,
        layers: &[PathBuf],
        prior: bool,
    ) -> Result<PathBuf, DaemonError> {
        crate::session_view::prepare(
            &self.views_dir,
            &format!(
                "{}-cohort-{}",
                session.slot,
                if prior { "prior" } else { "candidate" }
            ),
            layers,
        )
    }
    fn install_cohort_app_in_view(
        &mut self,
        session: &Session,
        tapp: &Path,
        layers: &[PathBuf],
    ) -> Result<String, DaemonError> {
        let view = match &self.cohort_view {
            Some((slot, existing, view)) if slot == &session.slot && existing == layers => {
                view.clone()
            }
            _ => {
                return Err(DaemonError::InvalidParams(
                    "cohort installer has no current request-scoped view".into(),
                ));
            }
        };
        let uid = self.account_uid(&session.user)?;
        let home = home_of(&session.user).ok_or_else(|| DaemonError::Apply {
            step: "select cohort in verified view".into(),
            detail: "the verified session account has no home".into(),
        })?;
        let environment = self.cohort_environment(session, uid)?;
        with_staged_app(tapp, |staged| {
            run(
                "select cohort in verified view",
                &mut cohort_view_command(session, uid, &home, &view, staged, &environment),
            )
        })
    }
    fn user_packages(
        &mut self,
        session: &Session,
    ) -> Result<Vec<package_store::registry::InstalledPackage>, DaemonError> {
        user_registry(&session.user)
    }

    fn image_has_runtime(&mut self, version: &str) -> bool {
        let relative = Path::new("usr/lib/tilo-runtime").join(version);
        if !Path::new("/").join(&relative).is_dir() {
            return false;
        }
        // `/usr` is the image's own unless systemd-sysext merged extensions
        // over it. The image's lower `/usr` is then out of reach, so the
        // directory counts as the image's only when no extension carries it.
        if !Path::new("/usr/.systemd-sysext").exists() {
            return true;
        }
        !SYSEXT_SEARCH_DIRS
            .iter()
            .filter_map(|dir| fs::read_dir(dir).ok())
            .flatten()
            .flatten()
            .any(|extension| extension.path().join(&relative).exists())
    }

    fn install_cohort_app(
        &mut self,
        session: &Session,
        tapp: &Path,
    ) -> Result<String, DaemonError> {
        install_app_in_session(self, session, tapp, true)
    }
}

fn install_app_in_session(
    apply: &mut SystemdApply,
    session: &Session,
    tapp: &Path,
    preserve_held: bool,
) -> Result<String, DaemonError> {
    // The installed runtime and publisher pins are part of the resolved
    // slot's verified /usr view, not the daemon's global namespace.
    let pid = apply
        .session_pid(session)
        .ok_or_else(|| DaemonError::Apply {
            step: "tilo-install".into(),
            detail: "the slot has no live session mount namespace".into(),
        })?;
    let user = &session.user;
    let uid = uid_of(user).ok_or_else(|| DaemonError::Apply {
        step: "tilo-install".into(),
        detail: format!("no user {user}"),
    })?;
    let home = home_of(user).unwrap_or_else(|| format!("/home/{user}"));
    // The slot user must read the package; the state root is root's.
    let dir = Path::new("/var/tmp").join(format!("tilo-vm-app-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).map_err(DaemonError::io("stage the app package"))?;
    let staged = dir.join("app.tapp");
    let result = fs::copy(tapp, &staged)
        .map_err(DaemonError::io("stage the app package"))
        .and_then(|_| {
            make_readable(&dir, &staged)?;
            run(
                "tilo-install install --allow-self-signed --force",
                &mut if preserve_held {
                    selected_app_install_command(pid, user, uid, &home, &staged, true)
                } else {
                    app_install_command(pid, user, uid, &home, &staged)
                },
            )
        });
    let _ = fs::remove_dir_all(&dir);
    result
}

fn with_staged_app(
    tapp: &Path,
    install: impl FnOnce(&Path) -> Result<String, DaemonError>,
) -> Result<String, DaemonError> {
    let dir = Path::new("/var/tmp").join(format!("tilo-vm-app-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&dir).map_err(DaemonError::io("stage the app package"))?;
    let staged = dir.join("app.tapp");
    let result = fs::copy(tapp, &staged)
        .map_err(DaemonError::io("stage the app package"))
        .and_then(|_| make_readable(&dir, &staged))
        .and_then(|()| install(&staged));
    let _ = fs::remove_dir_all(&dir);
    result
}

/// A transient system service gets the same readonly slot view as the final
/// session, independently of the old compositor's retained namespace. systemd
/// sets the existing session account; no caller command or identity is accepted.
fn cohort_view_command(
    session: &Session,
    uid: u32,
    home: &str,
    view: &Path,
    staged: &Path,
    environment: &[(String, String)],
) -> Command {
    let mut command = cohort_account_command(session, uid, home, view, environment);
    command
        .args([
            "install",
            "--allow-self-signed",
            "--force",
            "--preserve-held",
        ])
        .arg(staged);
    command
}

fn cohort_process_environment(pid: u32, uid: u32) -> Result<Vec<(String, String)>, DaemonError> {
    if !proc_field(pid, "Uid:").is_some_and(|value| {
        value
            .split_whitespace()
            .all(|value| value.parse::<u32>() == Ok(uid))
    }) {
        return Err(DaemonError::InvalidParams(
            "the session process account changed".into(),
        ));
    }
    let bytes = fs::read(format!("/proc/{pid}/environ"))
        .map_err(DaemonError::io("read admitted session environment"))?;
    Ok(crate::exec::session_env(&bytes))
}

fn cohort_account_command(
    session: &Session,
    uid: u32,
    home: &str,
    view: &Path,
    environment: &[(String, String)],
) -> Command {
    let mut command = Command::new("systemd-run");
    command
        .args(["--wait", "--pipe", "--collect", "--quiet"])
        .arg(format!(
            "--unit=tilo-vm-cohort-{}",
            uuid::Uuid::new_v4().simple()
        ))
        .arg(format!("--property=User={}", session.user))
        .arg(format!(
            "--property=BindReadOnlyPaths={}/usr:/usr {}/opt:/opt",
            view.display(),
            view.display()
        ))
        .arg(format!("--setenv=HOME={home}"))
        .arg(format!("--setenv=USER={}", session.user))
        .arg(format!("--setenv=XDG_RUNTIME_DIR=/run/user/{uid}"));
    for (key, value) in environment {
        if matches!(key.as_str(), "XDG_DATA_HOME" | "XDG_RUNTIME_DIR") {
            command.arg(format!("--setenv={key}={value}"));
        }
    }
    command.args([
        "--property=NoNewPrivileges=yes",
        "--property=RuntimeMaxSec=300",
        "--",
        "/usr/bin/tilo-install",
    ]);
    command
}

fn app_install_command(pid: u32, user: &str, uid: u32, home: &str, staged: &Path) -> Command {
    selected_app_install_command(pid, user, uid, home, staged, false)
}

fn selected_app_install_command(
    pid: u32,
    user: &str,
    uid: u32,
    home: &str,
    staged: &Path,
    preserve_held: bool,
) -> Command {
    let mut arguments = vec![
        "/usr/bin/tilo-install".into(),
        "install".into(),
        "--allow-self-signed".into(),
        "--force".into(),
    ];
    if preserve_held {
        arguments.push("--preserve-held".into());
    }
    arguments.push(staged.to_string_lossy().into_owned());
    crate::exec::in_session(
        pid,
        user,
        &[
            ("HOME".into(), home.into()),
            ("USER".into(), user.into()),
            ("XDG_RUNTIME_DIR".into(), format!("/run/user/{uid}")),
        ],
        &arguments,
    )
}

/// How long a login's compositor may take to exit after SIGTERM.
const DESKTOP_EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// How long the restarted compositor may take to come up.
const DESKTOP_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Wait until `wrapper` (a login's tilo-session) runs one compositor for
/// `settle`. The wrapper exiting means the compositor did not come up and
/// the owner is back at the login screen.
fn wait_desktop(wrapper: u32, settle: std::time::Duration) -> Result<(), String> {
    let started = std::time::Instant::now();
    let mut stable: Option<(std::time::Instant, u32)> = None;
    loop {
        if comm(wrapper).as_deref() != Some(LOGIN_WRAPPER) || zombie(wrapper) {
            return Err(
                "the login ended because its compositor did not keep running; \
                 the machine shows the login screen"
                    .into(),
            );
        }
        match (compositor_of(wrapper), stable) {
            (Some(pid), Some((since, seen))) if pid == seen => {
                if since.elapsed() >= settle {
                    return Ok(());
                }
            }
            (Some(pid), _) => stable = Some((std::time::Instant::now(), pid)),
            (None, _) => stable = None,
        }
        if started.elapsed() >= DESKTOP_START_TIMEOUT {
            return Err(format!(
                "no compositor kept running within {}s",
                DESKTOP_START_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Start `tilo-vmd input-serve` for `slot` as a transient unit and wait
/// until its socket answers and udev has handed the devices to the seat.
fn start_input_server(
    slot: &str,
    seat: &str,
    geometry: Option<input::Geometry>,
    socket: &Path,
) -> Result<(), DaemonError> {
    let unit = input::unit_name(slot);
    let _ = Command::new("systemctl").args(["stop", &unit]).output();
    let exe = std::env::current_exe().map_err(DaemonError::io("find the tilo-vmd binary"))?;
    let mut command = Command::new("systemd-run");
    command
        .arg(format!("--unit={unit}"))
        .args(["--collect", "--quiet"])
        .arg(format!(
            "--description=tilo-vm input devices for slot {slot}"
        ))
        .arg(exe)
        .args(["input-serve", "--seat", seat, "--socket"])
        .arg(socket);
    if let Some(input::Geometry {
        desktop: (width, height),
        touch: (touch_width, touch_height),
    }) = geometry
    {
        command.arg(format!("--extent={width}x{height}"));
        command.arg(format!("--touch-extent={touch_width}x{touch_height}"));
    }
    run(&format!("systemd-run --unit {unit}"), &mut command)?;
    for _ in 0..100 {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            // New input devices reach the compositor via udev and libinput.
            let _ = Command::new("udevadm").arg("settle").output();
            std::thread::sleep(std::time::Duration::from_millis(500));
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(DaemonError::Apply {
        step: format!("start {unit}"),
        detail: format!(
            "{} did not appear within 5s; journalctl -u {unit} says why",
            socket.display()
        ),
    })
}

/// The desktop layout under `~/.config/tilo`: the tree, its document, the
/// document's log and meta, and the action invocations recorded against it.
const DESKTOP_STATE: [&str; 5] = [
    "state.json",
    "state.automerge",
    "state.automerge.log",
    "state.automerge.meta",
    "state.json.invocations",
];

/// The rows of `user`'s own package registry, read as root from a file
/// the account owns, without following a link the account could plant.
fn user_registry(
    user: &str,
) -> Result<Vec<package_store::registry::InstalledPackage>, DaemonError> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    #[derive(serde::Deserialize)]
    struct RegistryFile {
        #[serde(default)]
        entry: Vec<package_store::registry::InstalledPackage>,
    }

    let failed = |detail: String| DaemonError::Apply {
        step: format!("read {user}'s package registry"),
        detail,
    };
    let (Some(home), Some(uid)) = (home_of(user), uid_of(user)) else {
        return Err(failed("the account has no home or uid".into()));
    };
    let path = Path::new(&home).join(".local/share/tilo-apps/registry.toml");
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(failed(format!("{}: {error}", path.display()))),
    };
    let metadata = file
        .metadata()
        .map_err(|error| failed(format!("{}: {error}", path.display())))?;
    if !metadata.is_file() || metadata.uid() != uid || metadata.len() > USER_REGISTRY_MAX_BYTES {
        return Err(failed(format!(
            "{} is not a bounded regular file the account owns",
            path.display()
        )));
    }
    let mut text = String::new();
    file.take(USER_REGISTRY_MAX_BYTES)
        .read_to_string(&mut text)
        .map_err(|error| failed(format!("{}: {error}", path.display())))?;
    toml::from_str::<RegistryFile>(&text)
        .map(|registry| registry.entry)
        .map_err(|error| failed(format!("{}: {error}", path.display())))
}

fn home_of(user: &str) -> Option<String> {
    let output = Command::new("getent")
        .args(["passwd", user])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&output.stdout).into_owned();
    line.split(':')
        .nth(5)
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
}

#[cfg(unix)]
fn make_readable(dir: &Path, file: &Path) -> Result<(), DaemonError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
        .and_then(|()| fs::set_permissions(file, fs::Permissions::from_mode(0o644)))
        .map_err(DaemonError::io("make the app package readable"))
}

#[cfg(not(unix))]
fn make_readable(_dir: &Path, _file: &Path) -> Result<(), DaemonError> {
    Ok(())
}

/// The active policy's `file_contexts`, or `None` when `SELinux` is off.
fn file_contexts() -> Option<PathBuf> {
    if !Path::new("/sys/fs/selinux/enforce").exists() {
        return None;
    }
    let config = fs::read_to_string("/etc/selinux/config").unwrap_or_default();
    let policy = config
        .lines()
        .find_map(|line| line.trim().strip_prefix("SELINUXTYPE="))
        .map_or("targeted", str::trim);
    let path = Path::new("/etc/selinux")
        .join(policy)
        .join("contexts/files/file_contexts");
    path.is_file().then_some(path)
}

/// One `Field:` line of `/proc/<pid>/status`, without the field name.
fn proc_field(pid: u32, field: &str) -> Option<String> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .map(|v| v.trim().to_owned())
}

/// One independent output per connector: each gets its own primary plane,
/// CRTC and encoder, so every output can run its own mode. The topology is
/// fixed at creation: vkms refuses changes while a device is enabled, so
/// hotplug means toggling a connector's `status` between connected (1) and
/// disconnected (2). vkms names the connectors `Virtual-1`, `Virtual-2`, ...
/// in this order. Link targets must be absolute.
fn vkms_create(device: &Path, connectors: u32) -> std::io::Result<()> {
    fs::create_dir(device)?;
    for index in 0..connectors.max(1) {
        let (plane, crtc, encoder, connector) = (
            device.join(format!("planes/p{index}")),
            device.join(format!("crtcs/c{index}")),
            device.join(format!("encoders/e{index}")),
            device.join(format!("connectors/n{index}")),
        );
        for dir in [&plane, &crtc, &encoder, &connector] {
            fs::create_dir(dir)?;
        }
        fs::write(plane.join("type"), "1")?;
        symlink(&crtc, &plane.join(format!("possible_crtcs/c{index}")))?;
        symlink(&crtc, &encoder.join(format!("possible_crtcs/c{index}")))?;
        symlink(
            &encoder,
            &connector.join(format!("possible_encoders/e{index}")),
        )?;
    }
    fs::write(device.join("enabled"), "1")
}

/// Disable, unlink and remove a vkms device. The caller has already stopped
/// the session that held it.
fn vkms_remove(device: &Path) -> std::io::Result<()> {
    fs::write(device.join("enabled"), "0")?;
    let mut entries = Vec::new();
    walk(device, Path::new(""), &mut entries)?;
    for entry in &entries {
        let path = device.join(entry);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            fs::remove_file(path)?;
        }
    }
    for group in ["connectors", "encoders", "crtcs", "planes"] {
        for child in fs::read_dir(device.join(group))? {
            fs::remove_dir(child?.path())?;
        }
    }
    fs::remove_dir(device)
}

/// The DRM card of vkms device `seat`: `/sys/devices/faux/<seat>/drm/cardN`.
fn vkms_card(seat: &str) -> Option<PathBuf> {
    let drm = Path::new("/sys/devices/faux").join(seat).join("drm");
    fs::read_dir(drm)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|card| {
            card.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("card"))
                .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_install_uses_the_verified_slot_namespace_and_never_ignores_runtime() {
        let command = super::app_install_command(
            123,
            "you",
            1002,
            "/var/home/you",
            std::path::Path::new("/var/tmp/private/app.tapp"),
        );
        assert_eq!(command.get_program(), "nsenter");
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[..2], ["--target=123", "--mount"]);
        assert!(
            args.windows(4)
                .any(|args| args == ["runuser", "-u", "you", "--"])
        );
        assert!(args.contains(&"HOME=/var/home/you".into()));
        assert!(args.contains(&"XDG_RUNTIME_DIR=/run/user/1002".into()));
        assert_eq!(
            &args[args.len() - 5..],
            [
                "/usr/bin/tilo-install",
                "install",
                "--allow-self-signed",
                "--force",
                "/var/tmp/private/app.tapp"
            ]
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("ignore-missing-runtime"))
        );
        let selected = super::selected_app_install_command(
            123,
            "you",
            1002,
            "/var/home/you",
            std::path::Path::new("/var/tmp/private/app.tapp"),
            true,
        );
        let selected: Vec<_> = selected
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&selected[..2], ["--target=123", "--mount"]);
        assert_eq!(
            &selected[selected.len() - 6..],
            [
                "/usr/bin/tilo-install",
                "install",
                "--allow-self-signed",
                "--force",
                "--preserve-held",
                "/var/tmp/private/app.tapp",
            ]
        );
        assert!(
            !selected
                .iter()
                .any(|arg| arg.contains("ignore-missing-runtime"))
        );
    }

    #[test]
    fn app_install_refuses_a_missing_session_without_host_fallback() {
        let root = tempfile::tempdir().unwrap();
        let session = super::Session {
            slot: "missing".into(),
            unit: "tilo-test-missing-install-session.service".into(),
            user: "missing-user".into(),
            units: vec![],
            control_socket: root.path().join("control"),
            stand_in: true,
            login: None,
        };
        let mut apply = super::SystemdApply::new(root.path());
        let error =
            super::Apply::install_app(&mut apply, &session, &root.path().join("missing.tapp"))
                .unwrap_err();
        assert!(
            matches!(error, super::DaemonError::Apply { step, detail } if step == "tilo-install" && detail == "the slot has no live session mount namespace")
        );
    }

    use super::*;

    #[test]
    fn changed_user_unit_is_reloaded_before_restart_and_reload_failure_stops_it() {
        let mut calls = Vec::new();
        let mut loaded_pin = false;
        try_restart_unit_with(Some("xaver"), "tilo-ush.service", |_, command| {
            assert_eq!(command.get_program(), "systemctl");
            let args: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert_eq!(&args[..2], ["--machine=xaver@", "--user"]);
            calls.push(args[2].clone());
            match args[2].as_str() {
                "daemon-reload" => loaded_pin = true,
                "show" => {
                    assert!(loaded_pin);
                    return Ok("loaded\n".into());
                }
                "try-restart" => {
                    assert!(
                        loaded_pin,
                        "restart must observe the newly layered service definition"
                    );
                    assert_eq!(args[3], "tilo-ush.service");
                }
                unexpected => panic!("unexpected user manager operation {unexpected}"),
            }
            Ok(String::new())
        })
        .unwrap();
        assert_eq!(calls, ["daemon-reload", "show", "try-restart"]);

        let mut calls = 0;
        let error = try_restart_unit_with(Some("xaver"), "tilo-ush.service", |_, _| {
            calls += 1;
            Err(DaemonError::Apply {
                step: "reload".into(),
                detail: "manager unavailable".into(),
            })
        });
        assert!(error.is_err());
        assert_eq!(calls, 1, "a failed reload cannot restart the stale unit");
    }

    #[test]
    fn deferred_greeter_activation_still_validates_the_installed_manager() {
        for loaded in ["loaded", "merged", "not-found", "masked", "error", ""] {
            let mut calls = Vec::new();
            let result = validate_unit_with(None, GREETER_UNIT, &mut |_, command| {
                calls.push(
                    command
                        .get_args()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect::<Vec<_>>(),
                );
                Ok(loaded.into())
            });
            assert_eq!(result.is_ok(), matches!(loaded, "loaded" | "merged"));
            assert_eq!(
                calls,
                [vec![
                    "show",
                    "--property=LoadState",
                    "--value",
                    GREETER_UNIT
                ]]
            );
        }
        let result = validate_unit_with(None, GREETER_UNIT, &mut |_, _| {
            Err(DaemonError::Apply {
                step: "manager unavailable".into(),
                detail: "failure".into(),
            })
        });
        assert!(
            result.is_err(),
            "manager failures cannot become a successful deferral"
        );
    }

    #[test]
    fn component_restart_validates_units_and_preserves_manager_failures() {
        for user in [None, Some("xaver")] {
            for state in ["not-found", "masked", "error", ""] {
                let mut tried = false;
                let result = try_restart_unit_with(user, "optional.service", |_, command| {
                    let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy()).collect();
                    if args.iter().any(|a| a == "try-restart") {
                        tried = true;
                    }
                    Ok(if args.iter().any(|a| a == "show") {
                        state.into()
                    } else {
                        String::new()
                    })
                });
                assert!(result.is_err(), "invalid installed-unit state {state}");
                assert!(
                    !tried,
                    "missing or unloadable units cannot be silently skipped"
                );
            }
            let error = try_restart_unit_with(user, "active.service", |step, command| {
                if command.get_args().any(|a| a == "try-restart") {
                    return Err(DaemonError::Apply {
                        step: step.into(),
                        detail: "restart denied".into(),
                    });
                }
                Ok("loaded".into())
            })
            .unwrap_err();
            assert!(error.to_string().contains("restart denied"));
        }
    }

    #[test]
    fn component_restart_uses_conditional_operation_for_both_managers() {
        for user in [None, Some("xaver")] {
            let mut verbs = Vec::new();
            try_restart_unit_with(user, "optional.service", |_, command| {
                let args: Vec<_> = command
                    .get_args()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                let offset = if user.is_some() { 2 } else { 0 };
                let verb = &args[offset];
                verbs.push(verb.clone());
                if verb == "show" {
                    assert_eq!(
                        &args[offset..],
                        [
                            "show",
                            "--property=LoadState",
                            "--value",
                            "optional.service"
                        ]
                    );
                    return Ok("loaded\n".into());
                }
                if verb == "try-restart" {
                    assert_eq!(
                        args.iter().skip(offset).nth(1).map(String::as_str),
                        Some("optional.service")
                    );
                } else {
                    assert_eq!(verb, "daemon-reload");
                }
                Ok(String::new())
            })
            .unwrap();
            assert_eq!(verbs.last().map(String::as_str), Some("try-restart"));
        }
    }

    /// The board session tilo-vmd looks for is the one the board layer
    /// installs, and the units and socket it names are the ones
    /// `tilo-seat-session` uses.
    #[test]
    fn the_device_session_is_the_board_layers_session() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let read = |path: &str| fs::read_to_string(repo.join(path)).unwrap();
        // Phase `the_names_match_the_board_layer`.
        {
            let layer = read("os/dev/install-board-layer.sh");
            assert!(
                layer.contains(&format!(": > {DEVICE_SESSION_MARKER}")),
                "marker"
            );
            let template = format!("os/usr/lib/systemd/system/{DEVICE_SESSION_TEMPLATE}.service");
            assert!(read(&template).contains("ExecStart=/usr/libexec/tilo-seat-session %i"));
            let seat = read("os/usr/libexec/tilo-seat-session");
            for unit in DEVICE_SESSION_UNITS {
                let name = unit.strip_suffix(".service").unwrap();
                assert!(seat.contains(&format!("drm_unit={name}\n")), "{unit}");
            }
            assert!(
                seat.contains(
                    r#"TILO_TEST_CONTROL="/run/user/$uid/tilo-session/test-control.sock""#
                )
            );
            assert_eq!(
                device_control_socket(1000),
                Path::new("/run/user/1000/tilo-session/test-control.sock")
            );
        }
        // Phase `the_active_instance_is_the_session`.
        {
            let listing = "tilo-board-headless-session@old.service loaded inactive dead Tilo board session for old\n\
                           tilo-board-headless-session@quickuser.service loaded active running Tilo board session for quickuser\n";
            assert_eq!(
                device_session_unit(listing).as_deref(),
                Some("tilo-board-headless-session@quickuser.service")
            );
            let stopped = "tilo-board-headless-session@quickuser.service loaded inactive dead x\n";
            assert_eq!(
                device_session_unit(stopped).as_deref(),
                Some("tilo-board-headless-session@quickuser.service")
            );
            assert_eq!(device_session_unit(""), None);
            assert_eq!(
                device_session_unit("tilo-board-headless-session@.service loaded"),
                None
            );
        }
    }

    /// On a laptop the device slot is the owner's greetd login: the session
    /// greetd's worker leads on seat0, never the greeter's or a slot's.
    #[test]
    fn the_device_session_of_a_laptop_is_the_owners_greetd_login() {
        // Phase `a_login_is_found_by_its_greetd_worker`.
        {
            let shown = |id: &str, text: &str| parse_logind_session(id, text);
            let greeter = shown(
                "1",
                "Name=greetd\nSeat=seat0\nClass=greeter\nRemote=no\nState=closing\nLeader=900\nScope=session-1.scope\n",
            );
            let ssh = shown(
                "2",
                "Name=quickuser\nSeat=\nClass=user\nRemote=yes\nState=active\nLeader=700\nScope=session-2.scope\n",
            );
            let slot = shown(
                "4",
                "Name=tds-a\nSeat=seat0\nClass=user\nRemote=no\nState=active\nLeader=800\nScope=session-4.scope\n",
            );
            let owner = shown(
                "3",
                "Name=quickuser\nSeat=seat0\nClass=user\nRemote=no\nState=active\nLeader=901\nScope=session-3.scope\n",
            );
            assert_eq!(owner.leader, 901);
            assert_eq!(owner.scope, "session-3.scope");
            // greetd is pid 500; its workers lead the greeter's and the owner's sessions.
            let parent = |pid: u32| match pid {
                900 | 901 => Some(500),
                _ => Some(1),
            };
            let sessions = [greeter, ssh, slot, owner];
            assert_eq!(
                greetd_login(&sessions, 500, parent).map(|s| s.id.as_str()),
                Some("3")
            );
            assert_eq!(greetd_login(&sessions[..3], 500, parent), None);
            assert_eq!(greetd_login(&sessions, 0, parent), None);
        }
        // Phase `the_names_match_tilo_session`.
        {
            let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
            let wrapper = repo.join("os/usr/libexec").join(LOGIN_WRAPPER);
            let script = fs::read_to_string(&wrapper).unwrap();
            assert!(
                script.contains("trap request_restart USR1"),
                "{}",
                wrapper.display()
            );
            assert!(script.contains("readonly SHELL_BIN=/usr/bin/shell"));
            assert_eq!(Path::new("/usr/bin/shell").file_name().unwrap(), COMPOSITOR);
            assert!(script.contains(
                r#"export TILO_TEST_CONTROL="$runtime_dir/tilo-session/test-control.sock""#
            ));
            let session =
                fs::read_to_string(repo.join("os/usr/share/wayland-sessions/shell.desktop"))
                    .unwrap();
            assert!(session.contains(&format!("Exec=/usr/libexec/{LOGIN_WRAPPER} ")));
        }
    }

    #[test]
    fn a_snapshot_is_read_once_the_compositor_has_written_all_of_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(b"\0\0\0\x0dIHDR-and-pixels");
        png.extend_from_slice(b"\0\0\0\0IEND\xae\x42\x60\x82");
        // Phase `only_a_whole_png_counts`.
        {
            assert!(png_complete(&png));
            assert!(!png_complete(&png[..png.len() - 1]));
            assert!(!png_complete(&png[..20]));
            assert!(!png_complete(b""));
        }
        // Phase `the_read_waits_for_the_frame_that_writes_the_file`.
        {
            let writer = {
                let (path, png) = (path.clone(), png.clone());
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    fs::write(&path, &png[..20]).unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    fs::write(&path, &png).unwrap();
                })
            };
            let read = await_png(&path, std::time::Duration::from_secs(10)).unwrap();
            writer.join().unwrap();
            assert_eq!(read, png);
        }
        // Phase `a_snapshot_that_never_lands_is_an_error_not_a_hang`.
        {
            fs::write(&path, &png[..20]).unwrap();
            let error = await_png(&path, std::time::Duration::from_millis(200)).unwrap_err();
            assert!(error.to_string().contains("no complete PNG"), "{error}");
        }
    }
}

#[cfg(test)]
mod userspace_tests {
    use super::*;

    #[test]
    fn cohort_store_environment_is_read_from_the_admitted_process() {
        use std::io::BufRead as _;
        use std::process::{Command, Stdio};
        let mut child = Command::new("sh")
            .args(["-c", "printf 'ready\\n'; read finish"])
            .env("XDG_DATA_HOME", "/srv/person/nondefault-data")
            .env("XDG_RUNTIME_DIR", "/run/person")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        let uid = super::proc_field(child.id(), "Uid:")
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<u32>()
            .unwrap();
        let environment = super::cohort_process_environment(child.id(), uid);
        let wrong_account = super::cohort_process_environment(child.id(), uid.wrapping_add(1));
        drop(child.stdin.take());
        child.wait().unwrap();
        let environment = environment.unwrap();
        assert!(
            environment.contains(&("XDG_DATA_HOME".into(), "/srv/person/nondefault-data".into()))
        );
        assert!(wrong_account.is_err());
        assert!(super::cohort_process_environment(child.id(), uid).is_err());
    }

    #[test]
    fn cohort_installer_uses_the_candidate_view_and_existing_account_without_restart() {
        let session = Session {
            slot: "owned".into(),
            user: "existing-person".into(),
            unit: "session-3.scope".into(),
            units: Vec::new(),
            control_socket: PathBuf::from("/run/owned/control"),
            stand_in: false,
            login: None,
        };
        let command = cohort_view_command(
            &session,
            1000,
            "/home/existing-person",
            Path::new("/verified/view"),
            Path::new("/staged/app.tapp"),
            &crate::exec::session_env(
                b"XDG_DATA_HOME=/srv/person/data\0XDG_RUNTIME_DIR=/run/person\0UNTRUSTED=bad\0",
            ),
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(command.get_program(), "systemd-run");
        for required in [
            "--property=User=existing-person",
            "--setenv=XDG_DATA_HOME=/srv/person/data",
            "--setenv=XDG_RUNTIME_DIR=/run/person",
            "--property=BindReadOnlyPaths=/verified/view/usr:/usr /verified/view/opt:/opt",
            "--property=NoNewPrivileges=yes",
            "--preserve-held",
            "/usr/bin/tilo-install",
            "/staged/app.tapp",
        ] {
            assert!(args.iter().any(|arg| arg == required), "{args:?}");
        }
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("nsenter") || arg.contains("restart"))
        );
    }
}
