//! [`Apply`] on a real dev image: users, systemd drop-ins, sysext, vkms seats.
//!
//! Everything here shells out to the same tools an administrator would use
//! (`useradd`, `systemctl`, `systemd-sysext`, `loginctl`, `journalctl`), so
//! what tilo-vmd does can be reproduced and inspected by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tilo_vm_protocol::{BootChange, BootConfirmResult, ControlResult, InputAction, OutputSpec};

use crate::apply::{Apply, Session, SessionProcess};
use crate::boot::Boot;
use crate::error::DaemonError;
use crate::fsutil::{symlink, walk, write_atomic};
use crate::input;
use crate::slots::DeviceSession;

const UNIT_DIR: &str = "/etc/systemd/system";
const SYSEXT_DIR: &str = "/var/lib/extensions";
const VKMS_CONFIGFS: &str = "/sys/kernel/config/vkms";
/// Not in production images; the dev image installs it.
const TILO_CONTROL: &str = "/usr/bin/tilo-control";
/// The groups `tilo-firstboot-user` gives a login, less `wheel`: `tilo-identity`
/// reads the machine's device key, which provisioning an identity needs, and
/// `video` opens `/dev/dri`.
const SLOT_USER_GROUPS: &str = "tilo-identity,video";

pub struct SystemdApply {
    layers_dir: PathBuf,
    boot: Boot,
}

impl SystemdApply {
    pub fn new(root: &Path) -> Self {
        Self {
            layers_dir: root.join("layers"),
            boot: Boot::new(root),
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

/// The compositor's control socket in the board session
/// (`TILO_TEST_CONTROL`, set by `tilo-seat-session`).
fn device_control_socket(uid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}/tilo-session/test-control.sock"))
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
            return None;
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
        })
    }

    fn remove_user(&mut self, user: &str) -> Result<(), DaemonError> {
        if uid_of(user).is_none() {
            return Ok(());
        }
        run("userdel", Command::new("userdel").args(["--remove", user])).map(drop)
    }

    fn write_dropin(&mut self, unit: &str, file: &str, contents: &str) -> Result<(), DaemonError> {
        write_atomic(
            &Path::new(UNIT_DIR).join(format!("{unit}.d")).join(file),
            contents.as_bytes(),
        )
    }

    fn remove_dropin(&mut self, unit: &str, file: &str) -> Result<(), DaemonError> {
        let dir = Path::new(UNIT_DIR).join(format!("{unit}.d"));
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
    fn link_sysext(&mut self, layers: &[PathBuf]) -> Result<(), DaemonError> {
        let dir = Path::new(SYSEXT_DIR);
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
            }
        }
        Ok(())
    }

    fn sysext_refresh(&mut self) -> Result<(), DaemonError> {
        run(
            "systemd-sysext refresh",
            Command::new("systemd-sysext").arg("refresh"),
        )
        .map(drop)
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

    fn wait_session_ready(&mut self, session: &Session) -> Result<(), String> {
        // Restart=always turns a failing start into active/activating flapping,
        // so the unit has to stay active under one main process for a while.
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);
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

    fn install_app(&mut self, user: &str, tapp: &Path) -> Result<String, DaemonError> {
        let uid = uid_of(user).ok_or_else(|| DaemonError::Apply {
            step: "tilo-install".into(),
            detail: format!("no user {user}"),
        })?;
        let home = home_of(user).unwrap_or_else(|| format!("/home/{user}"));
        // The slot user must read the package; the state root is root's.
        let dir =
            Path::new("/var/tmp").join(format!("tilo-vm-app-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&dir).map_err(DaemonError::io("stage the app package"))?;
        let staged = dir.join("app.tapp");
        let result = fs::copy(tapp, &staged)
            .map_err(DaemonError::io("stage the app package"))
            .and_then(|_| {
                make_readable(&dir, &staged)?;
                run(
                    "tilo-install install --allow-self-signed --force",
                    Command::new("runuser")
                        .args(["-u", user, "--", "env"])
                        .arg(format!("HOME={home}"))
                        .arg(format!("USER={user}"))
                        .arg(format!("XDG_RUNTIME_DIR=/run/user/{uid}"))
                        .args(["tilo-install", "install", "--allow-self-signed", "--force"])
                        .arg(&staged),
                )
            });
        let _ = fs::remove_dir_all(&dir);
        result
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

/// The desktop layout under `~/.config/tilo`: the tree, its document and
/// the document's meta, and the action invocations recorded against it.
const DESKTOP_STATE: [&str; 4] = [
    "state.json",
    "state.automerge",
    "state.automerge.meta",
    "state.json.invocations",
];

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
    use super::*;

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
