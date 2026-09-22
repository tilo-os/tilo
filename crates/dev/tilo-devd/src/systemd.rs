//! [`Apply`] on a real dev image: users, systemd drop-ins, sysext, vkms seats.
//!
//! Everything here shells out to the same tools an administrator would use
//! (`useradd`, `systemctl`, `systemd-sysext`, `loginctl`, `journalctl`), so
//! what tilo-devd does can be reproduced and inspected by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tilo_dev_protocol::ControlResult;

use crate::apply::Apply;
use crate::error::DaemonError;
use crate::fsutil::{symlink, walk, write_atomic};

const UNIT_DIR: &str = "/etc/systemd/system";
const SYSEXT_DIR: &str = "/var/lib/extensions";
const VKMS_CONFIGFS: &str = "/sys/kernel/config/vkms";
/// The dev image installs `tilo-control` next to the compositor.
const TILO_CONTROL: &str = "/usr/bin/tilo-control";

pub struct SystemdApply {
    layers_dir: PathBuf,
}

impl SystemdApply {
    pub fn new(root: &Path) -> Self {
        Self {
            layers_dir: root.join("layers"),
        }
    }
}

/// `/run/tilo-dev/<slot>`: the session unit's `RuntimeDirectory=`.
fn runtime_dir(slot: &str) -> PathBuf {
    Path::new("/run/tilo-dev").join(slot)
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

fn uid_of(user: &str) -> Option<u32> {
    let output = Command::new("getent")
        .args(["passwd", user])
        .output()
        .ok()?;
    let line = String::from_utf8_lossy(&output.stdout).into_owned();
    line.split(':').nth(2)?.trim().parse().ok()
}

impl Apply for SystemdApply {
    fn ensure_user(&mut self, user: &str) -> Result<u32, DaemonError> {
        if let Some(uid) = uid_of(user) {
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
                user,
            ]),
        )?;
        uid_of(user).ok_or_else(|| DaemonError::Apply {
            step: "useradd".into(),
            detail: format!("{user} has no uid after creation"),
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

    fn allocate_seat(&mut self, slot: &str) -> Result<String, DaemonError> {
        // The configfs device, the logind seat and the uinput `phys` tag share
        // one name; the shipped 72-vkms-seat.rules maps the card to the seat.
        let seat = format!("seat-{slot}");
        let device = Path::new(VKMS_CONFIGFS).join(&seat);
        if !device.exists() {
            run(
                "modprobe vkms",
                Command::new("modprobe").args(["vkms", "create_default_dev=0"]),
            )?;
            vkms_create(&device).map_err(DaemonError::io(format!("create vkms device {seat}")))?;
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
        slot: &str,
        user: &str,
        args: &[String],
    ) -> Result<ControlResult, DaemonError> {
        let socket = runtime_dir(slot).join("control.sock");
        let output = Command::new("runuser")
            .args(["-u", user, "--", "env"])
            .arg(format!("TILO_TEST_CONTROL={}", socket.display()))
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

    fn shot(&mut self, slot: &str, user: &str) -> Result<Vec<u8>, DaemonError> {
        let path = runtime_dir(slot).join(format!("shot-{}.png", uuid::Uuid::new_v4().simple()));
        let result = self.control(slot, user, &["snapshot".into(), path.display().to_string()])?;
        if result.exit_code != 0 {
            return Err(DaemonError::Apply {
                step: "tilo-control snapshot".into(),
                detail: result.stderr,
            });
        }
        let bytes = fs::read(&path).map_err(DaemonError::io("read snapshot"));
        let _ = fs::remove_file(&path);
        bytes
    }
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

/// Connectors per card. Fixed at creation: vkms refuses topology changes
/// while a device is enabled, so hotplug means toggling a connector's
/// `status` between connected (1) and disconnected (2).
const VKMS_CONNECTORS: u32 = 1;

/// One primary plane, CRTC and encoder, and [`VKMS_CONNECTORS`] connectors;
/// spare connectors start disconnected. Link targets must be absolute.
fn vkms_create(device: &Path) -> std::io::Result<()> {
    for dir in ["", "planes/p0", "crtcs/c0", "encoders/e0"] {
        fs::create_dir(device.join(dir))?;
    }
    fs::write(device.join("planes/p0/type"), "1")?;
    symlink(
        &device.join("crtcs/c0"),
        &device.join("planes/p0/possible_crtcs/c0"),
    )?;
    symlink(
        &device.join("crtcs/c0"),
        &device.join("encoders/e0/possible_crtcs/c0"),
    )?;
    for index in 0..VKMS_CONNECTORS {
        let connector = device.join(format!("connectors/n{index}"));
        fs::create_dir(&connector)?;
        symlink(
            &device.join("encoders/e0"),
            &connector.join("possible_encoders/e0"),
        )?;
        if index > 0 {
            fs::write(connector.join("status"), "2")?;
        }
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
