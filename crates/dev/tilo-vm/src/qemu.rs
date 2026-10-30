//! The one place a QEMU command line is built (`tilo-vm qemu`).
//!
//! Every headless boot of a Tilo disk goes through here: os/dev/vm.sh and the
//! release gates on top of it, the qemu adapter of `vm create`, the composefs
//! harness and the boot-timing checks. The machine, firmware, TPM, display,
//! input, network and console choices therefore live in one table, with the
//! reasons beside them, instead of drifting apart in each launcher.
//!
//! A VM's runtime files sit in one directory (see [`RunDir`]); os/dev/vm.sh
//! drives the monitor, console and serial log it finds there.

pub mod firmware;
pub mod serial;
pub mod swtpm;

use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use clap::{Args, ValueEnum};
use regex::Regex;
use serde::Serialize;
use serde_json::json;

use crate::app::Outcome;
use crate::error::CliError;

/// A refusal with its remedy.
#[derive(Debug)]
pub struct QemuError {
    pub detail: String,
    pub remedy: String,
}

impl QemuError {
    pub fn new(detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            remedy: remedy.into(),
        }
    }

    pub fn io(path: impl AsRef<Path>) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.as_ref().display().to_string();
        move |error| {
            Self::new(
                format!("{path}: {error}"),
                "check the path and its permissions",
            )
        }
    }
}

impl From<QemuError> for CliError {
    fn from(error: QemuError) -> Self {
        Self::Qemu {
            detail: error.detail,
            remedy: error.remedy,
        }
    }
}

/// A value inside a QEMU `-drive`/`-chardev` option list, where a comma
/// separates options and `,,` is a literal comma.
#[must_use]
pub fn escape(value: &str) -> String {
    value.replace(',', ",,")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
pub enum Arch {
    #[value(name = "x86_64", alias = "amd64")]
    #[serde(rename = "x86_64")]
    X86_64,
    #[value(name = "aarch64", alias = "arm64")]
    #[serde(rename = "aarch64")]
    Aarch64,
}

impl Arch {
    /// This machine's architecture, the guest's when none is named.
    #[must_use]
    pub fn host() -> Self {
        Self::parse(std::env::consts::ARCH).unwrap_or(Self::X86_64)
    }

    /// An architecture by any of its names (`uname -m`, Debian's, Rust's).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "x86_64" | "amd64" => Some(Self::X86_64),
            "aarch64" | "arm64" => Some(Self::Aarch64),
            _ => None,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }
}

/// How the guest CPU runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Accel {
    /// KVM when this environment can create a KVM VM for the guest, else
    /// QEMU's multithreaded emulator. The fallback is slower, but lets a gate
    /// run inside a CI runner that does not expose its node's /dev/kvm.
    Auto,
    Kvm,
    Tcg,
    /// Apple's Hypervisor.framework: a Homebrew QEMU on an Apple silicon Mac
    /// running an aarch64 guest (name its edk2 firmware with --aavmf-dir).
    Hvf,
}

/// The CPU a KVM guest is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CpuVariant {
    Host,
    /// The host CPU without SVE, to diagnose CPU feature selection on ARM.
    HostNoSve,
}

/// The guest's display. No single headless mode screenshots both a 2D
/// greeter and a GL compositor; pick per task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Display {
    /// No display device at all: firmware and kernel talk on the serial line.
    None,
    /// virtio VGA (x86) or virtio-gpu-pci (ARM), no host window. A monitor
    /// screendump captures firmware, the kernel console and the greeter; the
    /// compositor falls back to software GL (llvmpipe).
    #[value(name = "2d")]
    TwoD,
    /// The virtio GL device with egl-headless: the guest gets hardware GL
    /// (virgl), but a screendump only captures GL scanouts.
    Gl,
    /// The virtio GL device in a GTK window on this desktop.
    Window,
}

/// The guest's input devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Input {
    /// A USB tablet (and on ARM, which has no PC keyboard, a USB keyboard)
    /// for monitor `sendkey` and absolute pointer moves.
    Keyboard,
    /// A virtio multitouch screen and no keyboard at all (the PC machine's
    /// own i8042 is switched off too), as on a tablet with its keyboard
    /// detached; driven through the QMP socket in the run directory.
    Touch,
    None,
}

/// The NIC model on a `--lan` segment. e1000 implements Wake-on-LAN
/// settings, so a guest can enable magic-packet wake on it; virtio cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LanModel {
    #[value(name = "virtio-net-pci")]
    Virtio,
    E1000,
}

/// One virtio block disk, in the order given (the first is `/dev/vda`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    pub path: PathBuf,
    pub format: String,
    pub id: String,
    /// The guest's `/dev/disk/by-id` name. The installer binds a destructive
    /// target to a WWN or serial, which a virtio disk lacks unless given one.
    pub serial: Option<String>,
    pub readonly: bool,
    /// Create a blank qcow2 of this size when the file is absent.
    pub create: Option<String>,
    /// Create the file as a qcow2 child of this image when absent, so the
    /// image itself stays pristine.
    pub backing: Option<PathBuf>,
}

impl Disk {
    /// `PATH[,format=qcow2|raw][,serial=S][,id=ID][,readonly][,create=SIZE][,backing=IMAGE]`
    pub fn parse(spec: &str, index: usize) -> Result<Self, QemuError> {
        let mut parts = spec.split(',');
        let path = PathBuf::from(parts.next().unwrap_or_default());
        if path.as_os_str().is_empty() {
            return Err(QemuError::new(
                format!("--disk '{spec}' names no file"),
                "--disk PATH[,format=qcow2|raw][,serial=S][,id=ID][,readonly][,create=SIZE][,backing=IMAGE]",
            ));
        }
        let raw_by_name = matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("raw" | "img" | "iso")
        );
        let mut disk = Self {
            path,
            format: if raw_by_name { "raw" } else { "qcow2" }.into(),
            id: if index == 0 {
                "tilo-disk".into()
            } else {
                format!("tilo-disk{index}")
            },
            serial: None,
            readonly: false,
            create: None,
            backing: None,
        };
        for part in parts {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            match (key, value) {
                ("format", "qcow2" | "raw") => value.clone_into(&mut disk.format),
                ("serial", "") => disk.serial = None,
                ("serial", serial) => {
                    // The guest sees at most 20 bytes of it (virtio-blk's
                    // ID field); QEMU cuts the rest.
                    if !serial
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
                    {
                        return Err(QemuError::new(
                            format!("disk serial '{serial}' is not made of [A-Za-z0-9._-]"),
                            "choose a serial from those characters",
                        ));
                    }
                    disk.serial = Some(serial.into());
                }
                ("id", id)
                    if !id.is_empty()
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)) =>
                {
                    id.clone_into(&mut disk.id);
                }
                ("readonly", "" | "on") => disk.readonly = true,
                ("create", size) if !size.is_empty() => disk.create = Some(size.into()),
                ("backing", image) if !image.is_empty() => disk.backing = Some(image.into()),
                _ => {
                    return Err(QemuError::new(
                        format!("--disk '{spec}': '{part}' is not a disk option"),
                        "options: format=qcow2|raw, serial=S, id=ID, readonly, create=SIZE, backing=IMAGE",
                    ));
                }
            }
        }
        Ok(disk)
    }

    /// Create the file when it is absent and asked to be.
    pub fn prepare(&self, qemu_img: &str) -> Result<(), QemuError> {
        if self.path.exists() {
            return Ok(());
        }
        let mut command = Command::new(qemu_img);
        command.args(["create", "-q", "-f", "qcow2"]);
        if let Some(backing) = &self.backing {
            let format = image_format(backing);
            command.args(["-F", format, "-b"]).arg(backing);
        } else if self.create.is_none() {
            return Err(QemuError::new(
                format!("disk not found: {}", self.path.display()),
                "build the image first (os/dev/build-image.sh), or name an existing disk",
            ));
        }
        command.arg(&self.path);
        if let Some(size) = &self.create {
            command.arg(size);
        }
        run_quietly(&mut command, qemu_img)
    }

    fn args(&self) -> Vec<String> {
        let mut drive = format!(
            "file={},format={},if=none,id={}",
            escape(&self.path.display().to_string()),
            self.format,
            self.id
        );
        if self.readonly {
            drive.push_str(",readonly=on");
        }
        // The serial belongs to the virtio block device: QEMU rejects
        // serial= on -drive.
        let mut device = format!("virtio-blk-pci,drive={}", self.id);
        if let Some(serial) = &self.serial {
            device.push_str(",serial=");
            device.push_str(serial);
        }
        vec!["-drive".into(), drive, "-device".into(), device]
    }
}

/// `raw` for a raw image file by its name, else `qcow2`.
fn image_format(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("raw" | "img" | "iso") => "raw",
        _ => "qcow2",
    }
}

/// A point-to-point Ethernet segment to one other VM on this host, carried
/// as UDP between two loopback ports. It is a real shared L2 link (broadcasts
/// and ARP cross it), which user-mode networking, one private NAT per VM,
/// cannot give two guests. The peer VM gets the mirror image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lan {
    pub port: u16,
    pub peer: u16,
    pub mac: String,
    pub model: LanModel,
}

impl Lan {
    /// `PORT:PEER:MAC`, e.g. `23901:23902:52:54:00:23:83:0a`.
    pub fn parse(spec: &str, model: LanModel) -> Result<Self, QemuError> {
        let invalid = || {
            QemuError::new(
                format!("--lan '{spec}' is not PORT:PEER:MAC"),
                "give two loopback UDP ports and a lowercase 52:54:00:xx:xx:xx address, e.g. 23901:23902:52:54:00:23:83:0a",
            )
        };
        let mut parts = spec.splitn(3, ':');
        let port = parts
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p >= 1000);
        let peer = parts
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p >= 1000);
        let mac = parts.next().unwrap_or_default();
        let octets: Vec<&str> = mac.split(':').collect();
        let valid_mac = octets.len() == 6
            && mac.starts_with("52:54:00:")
            && octets.iter().all(|octet| {
                octet.len() == 2
                    && octet
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            });
        match (port, peer, valid_mac) {
            (Some(port), Some(peer), true) => Ok(Self {
                port,
                peer,
                mac: mac.into(),
                model,
            }),
            _ => Err(invalid()),
        }
    }

    fn args(&self) -> Vec<String> {
        let model = match self.model {
            LanModel::Virtio => "virtio-net-pci",
            LanModel::E1000 => "e1000",
        };
        vec![
            "-netdev".into(),
            format!(
                "socket,id=tilolan,udp=127.0.0.1:{},localaddr=127.0.0.1:{}",
                self.peer, self.port
            ),
            "-device".into(),
            format!("{model},netdev=tilolan,mac={}", self.mac),
        ]
    }
}

/// Where a VM's runtime files live. It must be short: `AF_UNIX` socket paths
/// are capped at 108 bytes.
#[derive(Debug, Clone)]
pub struct RunDir(pub PathBuf);

impl RunDir {
    #[must_use]
    pub fn monitor(&self) -> PathBuf {
        self.0.join("mon.sock")
    }
    #[must_use]
    pub fn serial(&self) -> PathBuf {
        self.0.join("serial.log")
    }
    /// The serial line as a socket something can type into (`--serial-socket`).
    #[must_use]
    pub fn serial_socket(&self) -> PathBuf {
        self.0.join("serial.sock")
    }
    #[must_use]
    pub fn qemu_log(&self) -> PathBuf {
        self.0.join("qemu.log")
    }
    #[must_use]
    pub fn pidfile(&self) -> PathBuf {
        self.0.join("qemu.pid")
    }
    /// The host end of the installer/greeter console channel.
    #[must_use]
    pub fn console(&self) -> PathBuf {
        self.0.join("console.sock")
    }
    #[must_use]
    pub fn qmp(&self) -> PathBuf {
        self.0.join("qmp.sock")
    }
    #[must_use]
    pub fn tpm(&self) -> PathBuf {
        self.0.join("tpm")
    }

    /// The running QEMU's pid, from its pidfile.
    #[must_use]
    pub fn running(&self) -> Option<u32> {
        let pid = std::fs::read_to_string(self.pidfile())
            .ok()?
            .trim()
            .parse()
            .ok()?;
        alive(pid).then_some(pid)
    }

    fn check_socket_length(&self) -> Result<(), QemuError> {
        let longest = self.serial_socket().display().to_string().len();
        if longest > 100 {
            return Err(QemuError::new(
                format!(
                    "the run directory {} is too deep for a Unix socket path (at most 108 bytes)",
                    self.0.display()
                ),
                "use a shorter --run directory, e.g. under $XDG_RUNTIME_DIR",
            ));
        }
        Ok(())
    }
}

/// Everything about the machine, as flags. os/dev/vm.sh maps its documented
/// TILO_* settings onto these.
#[derive(Debug, Clone, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent flag per QEMU feature"
)]
pub struct MachineArgs {
    /// The VM's runtime directory: monitor, serial log, console, pidfile,
    /// variable store and TPM state. Keep it short (Unix socket paths).
    #[arg(long)]
    pub run: Option<PathBuf>,
    /// QEMU's -name.
    #[arg(long, default_value = "tilo")]
    pub name: String,
    /// Guest architecture (default: this machine's).
    #[arg(long, value_enum)]
    pub arch: Option<Arch>,
    /// How the guest CPU runs: auto (KVM when this environment can create a
    /// KVM VM, else TCG emulation), kvm, tcg or hvf.
    #[arg(long, value_enum, default_value = "auto")]
    pub accel: Accel,
    /// The CPU a KVM guest gets: host, or host-no-sve to diagnose CPU feature selection.
    #[arg(long, value_enum, default_value = "host")]
    pub cpu_variant: CpuVariant,
    /// Guest memory in MiB.
    #[arg(long, default_value_t = 4096)]
    pub memory: u32,
    /// Guest CPUs.
    #[arg(long, default_value_t = 4)]
    pub smp: u32,
    /// The QEMU system emulator (default: qemu-system-<arch>).
    #[arg(long)]
    pub qemu: Option<String>,
    /// The qemu-img that creates overlays and blank disks.
    #[arg(long, default_value = "qemu-img")]
    pub qemu_img: String,
    /// A virtio disk, in boot order: PATH[,format=qcow2|raw][,serial=S][,id=ID][,readonly][,create=SIZE][,backing=IMAGE].
    #[arg(long = "disk")]
    pub disks: Vec<String>,
    /// none (serial only), 2d (screendumps of firmware and greeter), gl
    /// (virgl, egl-headless) or window (a GTK window here).
    #[arg(long, value_enum, default_value = "2d")]
    pub display: Display,
    /// The virtio-gpu mode (the compositor modesets the connector's
    /// preferred mode); bigger costs more software rendering.
    #[arg(long, default_value = "1280x800")]
    pub resolution: String,
    /// GL render node for --display gl (default: the first non-NVIDIA one).
    #[arg(long)]
    pub rendernode: Option<String>,
    /// keyboard (USB tablet, and a USB keyboard on ARM), touch (a virtio
    /// multitouch screen and no keyboard, driven through <run>/qmp.sock) or none.
    #[arg(long, value_enum, default_value = "keyboard")]
    pub input: Input,
    /// Attach a started software TPM 2.0 whose state persists in --tpm-dir.
    /// Pins the firmware to a build that measures, or refuses the boot.
    #[arg(long)]
    pub tpm: bool,
    /// The TPM's state directory (default: <run>/tpm).
    #[arg(long)]
    pub tpm_dir: Option<PathBuf>,
    /// Keep the pristine variable store across boots, so an enrolment in the
    /// guest (`/usr/libexec/tilo-secureboot enroll`) sticks.
    #[arg(long)]
    pub secboot: bool,
    /// The variable store's keys: none (setup mode), persist, microsoft, or
    /// the path of a prepared store.
    #[arg(long, default_value = "none")]
    pub keys: String,
    /// Keep the variable store across boots even with the default keys (one
    /// install cycle's boots share firmware state).
    #[arg(long)]
    pub keep_vars: bool,
    /// Offer the guest suspend-to-RAM, which the monitor's `system_wakeup`
    /// resumes (`x86_64` only; ARM `virt` has none).
    #[arg(long)]
    pub s3: bool,
    /// Where to look for OVMF (repeatable; default: the Fedora and Debian paths).
    #[arg(long = "ovmf-dir")]
    pub ovmf_dirs: Vec<PathBuf>,
    /// Where to look for AAVMF (repeatable; default: the Fedora and Debian paths).
    #[arg(long = "aavmf-dir")]
    pub aavmf_dirs: Vec<PathBuf>,
    /// Forward this host port on 127.0.0.1 to the guest's SSH port.
    #[arg(long)]
    pub ssh_port: Option<u16>,
    /// Another user-network forward, as QEMU's hostfwd value (repeatable),
    /// e.g. tcp:127.0.0.1:3341-:3340.
    #[arg(long = "forward")]
    pub forwards: Vec<String>,
    /// No network card at all.
    #[arg(long, conflicts_with_all = ["ssh_port", "forwards"])]
    pub no_net: bool,
    /// A second NIC on a UDP segment shared with one peer VM: PORT:PEER:MAC.
    #[arg(long)]
    pub lan: Option<String>,
    /// The --lan NIC: virtio-net-pci, or e1000 for Wake-on-LAN.
    #[arg(long, value_enum, default_value = "virtio-net-pci")]
    pub lan_model: LanModel,
    /// A sound card whose output QEMU writes to this WAV file.
    #[arg(long)]
    pub audio_wav: Option<PathBuf>,
    /// The tilo-control console channel (virtio-serial port
    /// org.tilo.console.0) the installer and greeter answer on, at
    /// <run>/console.sock.
    #[arg(long)]
    pub console: bool,
    /// A virtio-serial port the guest writes into a host file: NAME=PATH (repeatable).
    #[arg(long = "virtio-port")]
    pub virtio_ports: Vec<String>,
    /// Give root this public key, as the systemd credential
    /// `ssh.authorized_keys.root` in an SMBIOS type 11 OEM string.
    #[arg(long)]
    pub root_key: Option<PathBuf>,
    /// The serial line as a socket (<run>/serial.sock) something can type
    /// into, beside the serial log. Nothing has to be attached, but a client
    /// that connects must keep reading: QEMU blocks on a connected socket
    /// whose buffer is full, which stops the guest mid-boot.
    #[arg(long)]
    pub serial_socket: bool,
    /// Write the serial log here instead of <run>/serial.log.
    #[arg(long)]
    pub serial_log: Option<PathBuf>,
    /// Serve the display as VNC on this unix socket as well, so a remote
    /// client can watch the guest without a window on this host.
    #[arg(long)]
    pub vnc: Option<PathBuf>,
}

/// A validated machine.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent flag per QEMU feature"
)]
pub struct Machine {
    pub name: String,
    pub arch: Arch,
    pub accel: Accel,
    pub cpu_variant: CpuVariant,
    pub memory: u32,
    pub smp: u32,
    pub qemu: String,
    pub qemu_img: String,
    pub disks: Vec<Disk>,
    pub display: Display,
    pub resolution: (u32, u32),
    pub rendernode: Option<String>,
    pub input: Input,
    pub tpm: bool,
    pub tpm_dir: Option<PathBuf>,
    pub keep_vars: bool,
    pub firmware: firmware::Request,
    pub ssh_port: Option<u16>,
    pub forwards: Vec<String>,
    pub no_net: bool,
    pub lan: Option<Lan>,
    pub audio_wav: Option<PathBuf>,
    pub console: bool,
    pub virtio_ports: Vec<(String, PathBuf)>,
    pub root_key: Option<PathBuf>,
    pub serial_socket: bool,
    pub serial_log: Option<PathBuf>,
    pub vnc: Option<PathBuf>,
}

impl TryFrom<&MachineArgs> for Machine {
    type Error = QemuError;

    fn try_from(args: &MachineArgs) -> Result<Self, QemuError> {
        let arch = args.arch.unwrap_or_else(Arch::host);
        let resolution = args
            .resolution
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
            .filter(|(w, h)| *w > 1 && *h > 1)
            .ok_or_else(|| {
                QemuError::new(
                    format!("--resolution '{}' is not WIDTHxHEIGHT", args.resolution),
                    "e.g. --resolution 1280x800",
                )
            })?;
        if args.s3 && arch == Arch::Aarch64 {
            return Err(QemuError::new(
                "--s3: the ARM virt machine has no suspend-to-RAM",
                "drop --s3 for an aarch64 guest",
            ));
        }
        let disks = args
            .disks
            .iter()
            .enumerate()
            .map(|(index, spec)| Disk::parse(spec, index))
            .collect::<Result<Vec<_>, _>>()?;
        let virtio_ports = args
            .virtio_ports
            .iter()
            .map(|spec| {
                spec.split_once('=')
                    .filter(|(name, path)| {
                        !name.is_empty() && !path.is_empty() && !name.contains(',')
                    })
                    .map(|(name, path)| (name.to_owned(), PathBuf::from(path)))
                    .ok_or_else(|| {
                        QemuError::new(
                            format!("--virtio-port '{spec}' is not NAME=PATH"),
                            "e.g. --virtio-port org.tilo.vm-install-proof=/run/proof.txt",
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for forward in &args.forwards {
            if forward.contains(',') || !forward.contains('-') {
                return Err(QemuError::new(
                    format!("--forward '{forward}' is not one hostfwd value"),
                    "e.g. --forward tcp:127.0.0.1:3341-:3340 (repeat the flag for more)",
                ));
            }
        }
        Ok(Self {
            name: args.name.clone(),
            arch,
            accel: args.accel,
            cpu_variant: args.cpu_variant,
            memory: args.memory,
            smp: args.smp,
            qemu: args
                .qemu
                .clone()
                .unwrap_or_else(|| format!("qemu-system-{}", arch.name())),
            qemu_img: args.qemu_img.clone(),
            disks,
            display: args.display,
            resolution,
            rendernode: args.rendernode.clone(),
            input: args.input,
            tpm: args.tpm,
            tpm_dir: args.tpm_dir.clone(),
            keep_vars: args.keep_vars,
            firmware: firmware::Request {
                arch,
                tpm: args.tpm,
                secboot: args.secboot,
                keys: firmware::Keys::parse(&args.keys),
                s3: args.s3,
                ovmf_dirs: args.ovmf_dirs.clone(),
                aavmf_dirs: args.aavmf_dirs.clone(),
            },
            ssh_port: args.ssh_port,
            forwards: args.forwards.clone(),
            no_net: args.no_net,
            lan: args
                .lan
                .as_deref()
                .map(|spec| Lan::parse(spec, args.lan_model))
                .transpose()?,
            audio_wav: args.audio_wav.clone(),
            console: args.console,
            virtio_ports,
            root_key: args.root_key.clone(),
            serial_socket: args.serial_socket,
            serial_log: args.serial_log.clone(),
            vnc: args.vnc.clone(),
        })
    }
}

/// What the command line is built from, besides the machine.
pub struct Resolved<'a> {
    pub run: &'a RunDir,
    pub firmware: &'a firmware::Firmware,
    pub varstore: &'a Path,
    pub accel: &'a [String],
    pub rendernode: &'a str,
    /// The `-smbios` value carrying root's key.
    pub credential: Option<String>,
}

impl Machine {
    #[must_use]
    pub fn tpm(&self, run: &RunDir) -> swtpm::Swtpm {
        swtpm::Swtpm {
            dir: self.tpm_dir.clone().unwrap_or_else(|| run.tpm()),
        }
    }

    #[must_use]
    pub fn serial_log(&self, run: &RunDir) -> PathBuf {
        self.serial_log.clone().unwrap_or_else(|| run.serial())
    }

    fn machine_args(&self, smm: bool) -> Vec<String> {
        if self.arch == Arch::Aarch64 {
            return vec!["-machine".into(), "virt,gic-version=max".into()];
        }
        let keyboard = if self.input == Input::Touch {
            ",i8042=off"
        } else {
            ""
        };
        if smm {
            // SMM is what the Secure-Boot firmware's protected store needs;
            // the store and S3 resume do not coexist in OVMF, and leaving S3
            // on makes the firmware complain and fall back.
            vec![
                "-machine".into(),
                format!("q35,smm=on{keyboard}"),
                "-global".into(),
                "driver=cfi.pflash01,property=secure,value=on".into(),
                "-global".into(),
                "ICH9-LPC.disable_s3=1".into(),
            ]
        } else if self.firmware.s3 {
            vec![
                "-machine".into(),
                format!("q35{keyboard}"),
                "-global".into(),
                "ICH9-LPC.disable_s3=0".into(),
            ]
        } else {
            vec!["-machine".into(), format!("q35{keyboard}")]
        }
    }

    fn display_args(&self, rendernode: &str) -> Vec<String> {
        let (width, height) = self.resolution;
        let mode = format!("xres={width},yres={height}");
        let (plain, gl) = match self.arch {
            // QEMU's VGA-compatible virtio device is specific to PC machines.
            // ARM's virt board uses the PCI virtio-gpu, which AAVMF exposes
            // through GOP and the installer modesets the same way.
            Arch::X86_64 => ("virtio-vga", "virtio-vga-gl"),
            Arch::Aarch64 => ("virtio-gpu-pci", "virtio-gpu-gl-pci"),
        };
        match self.display {
            Display::None => vec![
                "-vga".into(),
                "none".into(),
                "-display".into(),
                "none".into(),
            ],
            Display::TwoD => vec![
                "-device".into(),
                format!("{plain},{mode}"),
                "-display".into(),
                "none".into(),
            ],
            Display::Gl => vec![
                "-device".into(),
                format!("{gl},{mode}"),
                "-display".into(),
                format!("egl-headless,rendernode={rendernode}"),
            ],
            Display::Window => vec![
                "-device".into(),
                format!("{gl},{mode}"),
                "-display".into(),
                "gtk,gl=on".into(),
            ],
        }
    }

    fn net_args(&self) -> Vec<String> {
        if self.no_net {
            return vec!["-nic".into(), "none".into()];
        }
        let mut nic = String::from("user,model=virtio-net-pci");
        if let Some(port) = self.ssh_port {
            let _ = write!(nic, ",hostfwd=tcp:127.0.0.1:{port}-:22");
        }
        for forward in &self.forwards {
            nic.push_str(",hostfwd=");
            nic.push_str(forward);
        }
        let mut args = vec!["-nic".into(), nic];
        if let Some(lan) = &self.lan {
            args.extend(lan.args());
        }
        args
    }

    fn input_args(&self, run: &RunDir) -> Vec<String> {
        let mut args: Vec<String> = match (self.input, self.arch) {
            (Input::Touch, _) => vec!["-device".into(), "virtio-multitouch-pci".into()],
            // ARM virt has no implicit PC keyboard; monitor sendkey needs a
            // guest device.
            (Input::Keyboard, Arch::Aarch64) => [
                "-device",
                "qemu-xhci",
                "-device",
                "usb-tablet",
                "-device",
                "usb-kbd",
            ]
            .map(String::from)
            .to_vec(),
            (Input::Keyboard, Arch::X86_64) => {
                ["-usb", "-device", "usb-tablet"].map(String::from).to_vec()
            }
            (Input::None, _) => return Vec::new(),
        };
        // QMP carries the absolute tablet's motion and buttons and the touch
        // screen's fingers: HMP's mouse_move is relative, which an absolute
        // tablet never sees.
        args.extend([
            "-qmp".into(),
            format!(
                "unix:{},server=on,wait=off",
                escape(&run.qmp().display().to_string())
            ),
        ]);
        args
    }

    /// The whole command line after the emulator.
    #[must_use]
    pub fn args(&self, resolved: &Resolved<'_>) -> Vec<String> {
        let run = resolved.run;
        let path = |p: &Path| escape(&p.display().to_string());
        let mut args = self.machine_args(resolved.firmware.smm);
        args.extend(resolved.accel.iter().cloned());
        args.extend([
            "-m".into(),
            self.memory.to_string(),
            "-smp".into(),
            self.smp.to_string(),
        ]);
        args.extend(resolved.firmware.pflash_args(resolved.varstore));
        for disk in &self.disks {
            args.extend(disk.args());
        }
        args.extend(self.display_args(resolved.rendernode));
        if let Some(vnc) = &self.vnc {
            // Beside whatever the display does locally: the guest renders
            // once, and the VNC server serves those same frames on a socket
            // this host's network never sees.
            args.extend(["-vnc".into(), format!("unix:{}", path(vnc))]);
        }
        args.extend(self.net_args());
        args.extend(self.input_args(run));
        if let Some(wav) = &self.audio_wav {
            args.extend([
                "-audiodev".into(),
                format!("wav,id=tiloaudio,path={}", path(wav)),
                "-device".into(),
                "virtio-sound-pci,audiodev=tiloaudio".into(),
            ]);
        }
        if self.console || !self.virtio_ports.is_empty() {
            args.extend(["-device".into(), "virtio-serial-pci".into()]);
        }
        if self.console {
            // The installer and the greeter answer the tilo-control action
            // protocol on this port; its host end is reachable only by the
            // user running QEMU. Physical machines have no virtio-serial bus.
            args.extend([
                "-chardev".into(),
                format!(
                    "socket,id=tiloconsole,path={},server=on,wait=off",
                    path(&run.console())
                ),
                "-device".into(),
                "virtserialport,chardev=tiloconsole,name=org.tilo.console.0".into(),
            ]);
        }
        for (index, (name, file)) in self.virtio_ports.iter().enumerate() {
            args.extend([
                "-chardev".into(),
                format!("file,id=tiloport{index},path={}", path(file)),
                "-device".into(),
                format!("virtserialport,chardev=tiloport{index},name={name}"),
            ]);
        }
        if let Some(credential) = &resolved.credential {
            args.extend(["-smbios".into(), credential.clone()]);
        }
        if self.serial_socket {
            // QEMU writes everything the guest says on the line to the log
            // too, whether or not anything is connected; with nothing
            // connected it goes only there.
            args.extend([
                "-chardev".into(),
                format!(
                    "socket,id=tiloserial,path={},server=on,wait=off,logfile={}",
                    path(&run.serial_socket()),
                    path(&self.serial_log(run))
                ),
            ]);
        }
        args.extend([
            "-monitor".into(),
            format!("unix:{},server,nowait", path(&run.monitor())),
            "-serial".into(),
            if self.serial_socket {
                "chardev:tiloserial".into()
            } else {
                format!("file:{}", path(&self.serial_log(run)))
            },
            "-pidfile".into(),
            run.pidfile().display().to_string(),
            "-name".into(),
            self.name.clone(),
        ]);
        if self.tpm {
            args.extend(self.tpm(run).qemu_args(self.arch));
        }
        args
    }

    /// The accelerator flags, and a warning when emulation stands in for KVM.
    pub fn accel_args(
        &self,
        kvm_usable: impl FnOnce() -> bool,
    ) -> Result<(Vec<String>, Option<String>), QemuError> {
        let kvm = || {
            let cpu = match self.cpu_variant {
                CpuVariant::Host => "host",
                CpuVariant::HostNoSve => "host,sve=off",
            };
            vec!["-enable-kvm".into(), "-cpu".into(), cpu.into()]
        };
        let tcg = || {
            ["-accel", "tcg,thread=multi", "-cpu", "max"]
                .map(String::from)
                .to_vec()
        };
        Ok(match self.accel {
            Accel::Kvm => (kvm(), None),
            Accel::Tcg => (tcg(), None),
            Accel::Hvf => (["-accel", "hvf", "-cpu", "host"].map(String::from).to_vec(), None),
            Accel::Auto if kvm_usable() => (kvm(), None),
            // Emulated CPU: boot and frame timings from this VM say nothing
            // about a KVM guest or hardware. Do not compare them.
            Accel::Auto => (
                tcg(),
                Some("no usable KVM here: this VM runs under TCG emulation, so its timings are not comparable with KVM or hardware".into()),
            ),
        })
    }

    /// The root key credential for `-smbios`.
    pub fn credential(&self) -> Result<Option<String>, QemuError> {
        let Some(path) = &self.root_key else {
            return Ok(None);
        };
        let text = std::fs::read_to_string(path).map_err(|error| {
            QemuError::new(
                format!(
                    "root public key {} is not readable: {error}",
                    path.display()
                ),
                "name a readable public key file",
            )
        })?;
        let public = text.lines().next().unwrap_or_default().trim();
        if public.is_empty() {
            return Err(QemuError::new(
                format!("root public key {} is empty", path.display()),
                "name a public key file with one key line",
            ));
        }
        Ok(Some(format!(
            "type=11,value={}",
            crate::sshkey::smbios_credential(public)
        )))
    }

    /// Resolve everything that depends on this host and print what was chosen.
    fn prepare(&self, run: &RunDir, seed: bool) -> Result<Prepared, QemuError> {
        let firmware = firmware::resolve(&self.firmware, &|path| path.exists())?;
        for note in &firmware.notes {
            eprintln!("[tilo-vm] {note}");
        }
        let varstore = if seed {
            firmware.seed_varstore(&run.0, self.keep_vars)?
        } else {
            firmware.varstore_path(&run.0)
        };
        let (accel, warning) = self.accel_args(|| kvm_usable(self.arch))?;
        if let Some(warning) = warning {
            eprintln!("[tilo-vm] warning: {warning}");
        }
        let rendernode = self.rendernode.clone().unwrap_or_else(detect_rendernode);
        let credential = self.credential()?;
        Ok(Prepared {
            firmware,
            varstore,
            accel,
            rendernode,
            credential,
        })
    }
}

/// Make the run directory, refusing one whose VM still runs: nothing of it
/// (its variable store least of all) may change under a running guest.
fn claim(run: &RunDir) -> Result<(), QemuError> {
    run.check_socket_length()?;
    std::fs::create_dir_all(&run.0).map_err(QemuError::io(&run.0))?;
    if let Some(pid) = run.running() {
        return Err(QemuError::new(
            format!("a VM is already running in {} (pid {pid})", run.0.display()),
            format!("stop it first: tilo-vm qemu stop --run {}", run.0.display()),
        ));
    }
    Ok(())
}

struct Prepared {
    firmware: firmware::Firmware,
    varstore: PathBuf,
    accel: Vec<String>,
    rendernode: String,
    credential: Option<String>,
}

impl Prepared {
    fn resolved<'a>(&'a self, run: &'a RunDir) -> Resolved<'a> {
        Resolved {
            run,
            firmware: &self.firmware,
            varstore: &self.varstore,
            accel: &self.accel,
            rendernode: &self.rendernode,
            credential: self.credential.clone(),
        }
    }
}

/// A GL render node for virglrenderer. NVIDIA (0x10de) is skipped: its EGL
/// stack works poorly with virglrenderer; a Mesa iGPU is strongly preferred.
fn detect_rendernode() -> String {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    nodes
        .into_iter()
        .find(|node| {
            let name = node
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            std::fs::read_to_string(format!("/sys/class/drm/{name}/device/vendor"))
                .map_or(true, |vendor| vendor.trim() != "0x10de")
        })
        .map_or_else(
            || "/dev/dri/renderD128".into(),
            |node| node.display().to_string(),
        )
}

/// Can this environment create a KVM VM for `arch`? A container can expose
/// a /dev/kvm node while denying `KVM_CREATE_VM`, so the node is not enough.
#[cfg(target_os = "linux")]
fn kvm_usable(arch: Arch) -> bool {
    use std::os::fd::AsRawFd;
    const KVM_GET_API_VERSION: libc::c_ulong = 0xAE00;
    const KVM_CREATE_VM: libc::c_ulong = 0xAE01;
    if arch != Arch::host() {
        return false;
    }
    let Ok(kvm) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
    else {
        return false;
    };
    // SAFETY: both requests take no argument pointer; the descriptor is open
    // for the duration of the calls.
    let version = unsafe { libc::ioctl(kvm.as_raw_fd(), KVM_GET_API_VERSION, 0) };
    if version != 12 {
        return false;
    }
    // SAFETY: as above; a successful call returns a new descriptor we close.
    let vm = unsafe { libc::ioctl(kvm.as_raw_fd(), KVM_CREATE_VM, 0) };
    if vm < 0 {
        return false;
    }
    // SAFETY: `vm` is a descriptor this function owns and closes once.
    unsafe { libc::close(vm) };
    true
}

#[cfg(not(target_os = "linux"))]
fn kvm_usable(_arch: Arch) -> bool {
    false
}

/// Put `command` in its own session, so it outlives the command that
/// started it and no terminal's hangup or interrupt reaches it.
#[cfg(unix)]
pub(crate) fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: setsid is async-signal-safe and touches no memory of the parent.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub(crate) fn detach(_command: &mut Command) {}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
pub(crate) fn terminate(pid: u32) {
    if let Ok(pid) = libc::pid_t::try_from(pid) {
        // SAFETY: sends SIGTERM to a process this tool started.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

#[cfg(not(unix))]
pub(crate) fn terminate(_pid: u32) {}

/// Is `tool` an executable on PATH (or a path that exists)?
pub(crate) fn on_path(tool: &str) -> bool {
    if tool.contains('/') {
        return Path::new(tool).exists();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(tool).is_file()))
}

fn run_quietly(command: &mut Command, what: &str) -> Result<(), QemuError> {
    let output = command.output().map_err(|error| {
        QemuError::new(
            format!("{what} could not run: {error}"),
            format!("install {what}"),
        )
    })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(QemuError::new(
            format!(
                "{what} failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            "its own message names the cause",
        ))
    }
}

/// Send one command to the human monitor and return what it answered.
fn monitor(socket: &Path, command: &str) -> std::io::Result<String> {
    #[cfg(unix)]
    {
        let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        stream.write_all(format!("{command}\n").as_bytes())?;
        let mut reply = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => reply.extend(buffer.get(..read).unwrap_or_default()),
            }
        }
        Ok(String::from_utf8_lossy(&reply).into_owned())
    }
    #[cfg(not(unix))]
    {
        let _ = (socket, command);
        Err(std::io::Error::other(
            "the QEMU monitor socket needs a Unix host",
        ))
    }
}

fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read(path)
        .map(|raw| serial::clean(&raw))
        .unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all.get(all.len().saturating_sub(lines)..)
        .unwrap_or_default()
        .join("\n")
}

fn shell_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(|word| {
            if !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b",./:=_-+@%".contains(&b))
            {
                word.to_owned()
            } else {
                format!("'{}'", word.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn require_run(args: &MachineArgs) -> Result<RunDir, QemuError> {
    args.run.clone().map(RunDir).ok_or_else(|| {
        QemuError::new(
            "no --run directory",
            "name the VM's runtime directory, e.g. --run \"$XDG_RUNTIME_DIR/tilo-vm\"",
        )
    })
}

/// `tilo-vm qemu args`: the command line, without starting anything.
pub fn args_command(args: &MachineArgs) -> Result<Outcome, CliError> {
    let machine = Machine::try_from(args)?;
    let run = args
        .run
        .clone()
        .map_or_else(|| RunDir(std::env::temp_dir().join("tilo-vm")), RunDir);
    let prepared = machine.prepare(&run, false)?;
    let command_line = machine.args(&prepared.resolved(&run));
    Ok(Outcome {
        text: shell_line(&machine.qemu, &command_line),
        data: json!({ "qemu": machine.qemu, "args": command_line }),
        next: Vec::new(),
        failed: false,
    })
}

/// `tilo-vm qemu firmware`: what the firmware settings resolve to.
pub fn firmware_command(args: &MachineArgs) -> Result<Outcome, CliError> {
    let machine = Machine::try_from(args)?;
    let firmware = firmware::resolve(&machine.firmware, &|path| path.exists())?;
    for note in &firmware.notes {
        eprintln!("[tilo-vm] {note}");
    }
    Ok(Outcome {
        text: firmware.table(machine.tpm),
        data: json!({
            "code": firmware.code,
            "vars": firmware.vars,
            "format": firmware.format,
            "keys": firmware.keys.name(),
            "measures": firmware.measures,
            "smm": firmware.smm,
            "tpm": machine.tpm,
        }),
        next: Vec::new(),
        failed: false,
    })
}

/// Start QEMU for `machine` with its runtime files in `run`; the child.
fn spawn(
    machine: &Machine,
    run: &RunDir,
    prepared: &Prepared,
    background: bool,
) -> Result<Child, QemuError> {
    for disk in &machine.disks {
        disk.prepare(&machine.qemu_img)?;
    }
    for stale in [
        run.monitor(),
        run.pidfile(),
        run.console(),
        run.qmp(),
        run.serial_socket(),
        machine.serial_log(run),
    ] {
        let _ = std::fs::remove_file(stale);
    }
    if machine.tpm {
        machine.tpm(run).start()?;
    }
    let argv = machine.args(&prepared.resolved(run));
    let log = std::fs::File::create(run.qemu_log()).map_err(QemuError::io(run.qemu_log()))?;
    let mut command = Command::new(&machine.qemu);
    command
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(QemuError::io(run.qemu_log()))?)
        .stderr(log);
    if background {
        detach(&mut command);
    }
    eprintln!("[tilo-vm] {}", shell_line(&machine.qemu, &argv));
    command.spawn().map_err(|error| {
        QemuError::new(
            format!("{} could not start: {error}", machine.qemu),
            format!(
                "install QEMU for {} (or name the emulator with --qemu)",
                machine.arch.name()
            ),
        )
    })
}

/// `tilo-vm qemu boot`: start the VM in the background and return once its
/// monitor answers.
pub fn boot_command(args: &MachineArgs, start_timeout: u64) -> Result<Outcome, CliError> {
    let machine = Machine::try_from(args)?;
    let run = require_run(args)?;
    claim(&run)?;
    let prepared = machine.prepare(&run, true)?;
    let mut child = spawn(&machine, &run, &prepared, true)?;
    // Return only once QEMU has done its cold-start work and published the
    // monitor: a caller (or a distrobox scope) that ends sooner can take a
    // healthy QEMU down with it. One that never opens its monitor failed.
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(start_timeout))
        .unwrap_or_else(Instant::now);
    loop {
        if run.monitor().exists() {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(QemuError::new(
                format!(
                    "QEMU exited at start ({status}); {}:\n{}",
                    run.qemu_log().display(),
                    tail(&run.qemu_log(), 40)
                ),
                "its log above names the cause",
            )
            .into());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(QemuError::new(
                format!(
                    "QEMU opened no monitor within {start_timeout}s; {}:\n{}",
                    run.qemu_log().display(),
                    tail(&run.qemu_log(), 40)
                ),
                "a cold, large image can take long to open: raise --start-timeout, or read the log",
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let pid = child.id();
    Ok(Outcome {
        text: format!(
            "up. monitor={} serial={}",
            run.monitor().display(),
            machine.serial_log(&run).display()
        ),
        data: json!({
            "pid": pid,
            "run": run.0,
            "monitor": run.monitor(),
            "serial": machine.serial_log(&run),
            "console": machine.console.then(|| run.console()),
            "firmware": prepared.firmware.code,
            "varstore": prepared.varstore,
        }),
        next: Vec::new(),
        failed: false,
    })
}

/// What `tilo-vm qemu run` waits for.
#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// Serial output that means the boot is ready (a regex; repeatable). The
    /// first match ends the run successfully.
    #[arg(long)]
    pub ready: Vec<String>,
    /// Serial output that means the boot failed (a regex; repeatable). It
    /// wins over --ready, wherever it appears.
    #[arg(long)]
    pub fail: Vec<String>,
    /// Seconds to wait; 0 waits until QEMU exits.
    #[arg(long, default_value_t = 300)]
    pub timeout: u64,
    /// Require a kernel timestamp at or before the ready line and report it
    /// as the boot time (without one the run fails rather than invent one).
    #[arg(long)]
    pub kernel_time: bool,
    /// Boot a throwaway qcow2 child of the first disk, so it stays
    /// byte-for-byte unchanged.
    #[arg(long)]
    pub overlay: bool,
    /// Seconds a guest gets to power off after the ACPI request.
    #[arg(long, default_value_t = 20)]
    pub shutdown_timeout: u64,
    /// Also write the result as JSON here.
    #[arg(long)]
    pub report: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct RunReport {
    state: String,
    pattern: Option<String>,
    line: Option<String>,
    elapsed_ms: u64,
    kernel_ms: Option<u64>,
    qemu_exit: Option<i32>,
    serial: Option<PathBuf>,
}

/// `tilo-vm qemu run`: boot in the foreground until the serial line says
/// ready or failed, QEMU exits, or time runs out; then power the guest off.
pub fn run_command(args: &MachineArgs, wait: &RunArgs) -> Result<Outcome, CliError> {
    let mut machine = Machine::try_from(args)?;
    let ready = compile(&wait.ready)?;
    let fail = compile(&wait.fail)?;
    let (run, scratch) = foreground_run_dir(args.run.as_deref())?;
    claim(&run)?;
    let overlay = run.0.join("overlay.qcow2");
    if wait.overlay {
        boot_overlay(&mut machine, &overlay)?;
    }
    let prepared = machine.prepare(&run, true)?;
    let serial_log = machine.serial_log(&run);
    let started = Instant::now();
    let mut child = spawn(&machine, &run, &prepared, false)?;
    let deadline = (wait.timeout > 0)
        .then(|| started.checked_add(Duration::from_secs(wait.timeout)))
        .flatten();
    let (state, hit, exit) = watch(
        &mut child,
        &serial_log,
        &ready,
        &fail,
        wait.kernel_time,
        deadline,
    );
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let exit = exit.or_else(|| power_off(&mut child, &run.monitor(), wait.shutdown_timeout));
    if machine.tpm {
        machine.tpm(&run).stop();
    }
    if wait.overlay {
        let _ = std::fs::remove_file(&overlay);
    }
    let passed = match state {
        "ready" => true,
        "exited" => ready.is_empty() && exit == Some(0),
        _ => false,
    };
    if !passed {
        eprintln!(
            "[tilo-vm] boot {state}; last serial lines ({}):\n{}",
            serial_log.display(),
            tail(&serial_log, 40)
        );
        if exit.is_some_and(|code| code != 0) {
            eprintln!(
                "[tilo-vm] {}:\n{}",
                run.qemu_log().display(),
                tail(&run.qemu_log(), 20)
            );
        }
    }
    let kept_serial = scratch.is_none() || machine.serial_log.is_some();
    let report = RunReport {
        state: state.into(),
        pattern: hit.as_ref().map(|h| h.pattern.clone()),
        line: hit.as_ref().map(|h| h.line.clone()),
        elapsed_ms,
        kernel_ms: hit.as_ref().and_then(|h| h.kernel_ms),
        qemu_exit: exit,
        serial: kept_serial.then(|| serial_log.clone()),
    };
    let data = serde_json::to_value(&report).unwrap_or_default();
    if let Some(path) = &wait.report {
        std::fs::write(path, format!("{data:#}\n")).map_err(QemuError::io(path))?;
    }
    let mut text = format!("{state} after {elapsed_ms} ms");
    if let Some(kernel_ms) = report.kernel_ms {
        let _ = write!(text, " (kernel time {kernel_ms} ms)");
    }
    if let Some(line) = &report.line {
        let _ = write!(text, ": {line}");
    }
    Ok(Outcome {
        text,
        data,
        next: Vec::new(),
        failed: !passed,
    })
}

/// The run directory of a foreground run: the one named, or a private
/// temporary one removed when the run ends.
fn foreground_run_dir(
    named: Option<&Path>,
) -> Result<(RunDir, Option<tempfile::TempDir>), QemuError> {
    if let Some(named) = named {
        return Ok((RunDir(named.to_path_buf()), None));
    }
    let scratch = tempfile::Builder::new()
        .prefix("tilo-qemu-")
        .tempdir_in(
            std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from),
        )
        .map_err(QemuError::io("temporary run directory"))?;
    Ok((RunDir(scratch.path().to_path_buf()), Some(scratch)))
}

fn compile(patterns: &[String]) -> Result<Vec<Regex>, QemuError> {
    patterns
        .iter()
        .map(|p| {
            Regex::new(p).map_err(|error| {
                QemuError::new(format!("'{p}' is not a regex: {error}"), "fix the pattern")
            })
        })
        .collect()
}

/// Boot a throwaway qcow2 child of the first disk instead of the disk.
fn boot_overlay(machine: &mut Machine, overlay: &Path) -> Result<(), QemuError> {
    let first = machine
        .disks
        .first_mut()
        .ok_or_else(|| QemuError::new("--overlay needs a --disk", "name the disk to boot"))?;
    let _ = std::fs::remove_file(overlay);
    let mut create = Command::new(&machine.qemu_img);
    create
        .args(["create", "-q", "-f", "qcow2", "-F", &first.format, "-b"])
        .arg(&first.path)
        .arg(overlay);
    run_quietly(&mut create, &machine.qemu_img)?;
    overlay.clone_into(&mut first.path);
    "qcow2".clone_into(&mut first.format);
    first.readonly = false;
    Ok(())
}

/// Watch the serial log until it says ready or failed, QEMU exits, or the
/// deadline passes: the state, what matched, and QEMU's exit code if it exited.
fn watch(
    child: &mut Child,
    serial_log: &Path,
    ready: &[Regex],
    fail: &[Regex],
    kernel_time: bool,
    deadline: Option<Instant>,
) -> (&'static str, Option<serial::Hit>, Option<i32>) {
    loop {
        let text = std::fs::read(serial_log)
            .map(|raw| serial::clean(&raw))
            .unwrap_or_default();
        if let Some(hit) = serial::scan(&text, ready, fail, kernel_time) {
            let state = match hit.verdict {
                serial::Verdict::Ready => "ready",
                serial::Verdict::Failed => "failed",
                serial::Verdict::MissingTiming => "missing-timing",
            };
            return (state, Some(hit), None);
        }
        if let Ok(Some(status)) = child.try_wait() {
            return ("exited", None, status.code());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return ("timeout", None, None);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Ask the guest to power off (an ACPI request through the monitor needs no
/// guest login), kill QEMU if it is still there after `timeout` seconds, and
/// return its exit code.
fn power_off(child: &mut Child, monitor_socket: &Path, timeout: u64) -> Option<i32> {
    if matches!(child.try_wait(), Ok(None)) {
        let _ = monitor(monitor_socket, "system_powerdown");
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(timeout))
            .unwrap_or_else(Instant::now);
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
        }
        let _ = child.kill();
    }
    child.wait().ok().and_then(|status| status.code())
}

/// `tilo-vm qemu stop`: power the VM in `run` off at once and stop its TPM.
pub fn stop_command(run: &Path, tpm_dir: Option<&Path>) -> Outcome {
    let run = RunDir(run.to_path_buf());
    if run.running().is_some() {
        let _ = monitor(&run.monitor(), "quit");
        std::thread::sleep(Duration::from_secs(1));
    }
    if let Some(pid) = run.running() {
        terminate(pid);
    }
    let _ = std::fs::remove_file(run.pidfile());
    swtpm::Swtpm {
        dir: tpm_dir.map_or_else(|| run.tpm(), Path::to_path_buf),
    }
    .stop();
    Outcome {
        text: "stopped".into(),
        data: json!({ "run": run.0, "running": false }),
        next: Vec::new(),
        failed: false,
    }
}

/// `tilo-vm qemu status`: is the VM in `run` running?
#[must_use]
pub fn status_command(run: &Path) -> Outcome {
    let pid = RunDir(run.to_path_buf()).running();
    Outcome {
        text: pid.map_or_else(|| "stopped".into(), |pid| format!("running (pid {pid})")),
        data: json!({ "run": run, "running": pid.is_some(), "pid": pid }),
        next: Vec::new(),
        failed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    #[command(args_override_self = true)]
    struct Probe {
        #[command(flatten)]
        machine: MachineArgs,
    }

    fn machine(flags: &[&str]) -> Machine {
        let args = Probe::parse_from(
            ["probe", "--arch", "x86_64"]
                .into_iter()
                .chain(flags.iter().copied()),
        )
        .machine;
        Machine::try_from(&args).unwrap()
    }

    fn firmware(smm: bool) -> firmware::Firmware {
        firmware::Firmware {
            code: "/fw/CODE.qcow2".into(),
            vars: "/fw/VARS.qcow2".into(),
            format: "qcow2",
            keys: firmware::Keys::None,
            measures: true,
            smm,
            notes: Vec::new(),
        }
    }

    fn line(machine: &Machine, smm: bool, credential: Option<String>) -> String {
        let run = RunDir("/run/t".into());
        let firmware = firmware(smm);
        let accel = machine.accel_args(|| true).unwrap().0;
        machine
            .args(&Resolved {
                run: &run,
                firmware: &firmware,
                varstore: Path::new("/run/t/vars.img"),
                accel: &accel,
                rendernode: "/dev/dri/renderD129",
                credential,
            })
            .join(" ")
    }

    #[test]
    fn x86_and_arm_get_their_native_machines_and_devices() {
        let x86 = machine(&["--arch", "amd64", "--disk", "/d/disk.qcow2"]);
        assert_eq!(x86.qemu, "qemu-system-x86_64");
        let text = line(&x86, false, None);
        assert!(
            text.starts_with("-machine q35 -enable-kvm -cpu host -m 4096 -smp 4 "),
            "{text}"
        );
        assert!(
            text.contains("-device virtio-vga,xres=1280,yres=800 -display none"),
            "{text}"
        );
        assert!(
            text.contains("-usb -device usb-tablet -qmp unix:/run/t/qmp.sock,server=on,wait=off"),
            "the tablet is driven through QMP: {text}"
        );
        assert!(text.contains(
            "-drive file=/d/disk.qcow2,format=qcow2,if=none,id=tilo-disk -device virtio-blk-pci,drive=tilo-disk"
        ));

        let arm = machine(&["--arch", "arm64", "--accel", "tcg"]);
        assert_eq!(arm.qemu, "qemu-system-aarch64");
        let text = line(&arm, false, None);
        assert!(
            text.starts_with("-machine virt,gic-version=max -accel tcg,thread=multi -cpu max"),
            "{text}"
        );
        assert!(
            text.contains("-device virtio-gpu-pci,xres=1280,yres=800"),
            "{text}"
        );
        assert!(
            text.contains("-device qemu-xhci -device usb-tablet -device usb-kbd"),
            "{text}"
        );
        // No machine string picks an accelerator of its own.
        assert!(!text.contains("accel=kvm"));
    }

    #[test]
    fn secure_boot_firmware_gets_smm_and_a_protected_pflash() {
        let text = line(&machine(&[]), true, None);
        assert!(text.starts_with(
            "-machine q35,smm=on -global driver=cfi.pflash01,property=secure,value=on -global ICH9-LPC.disable_s3=1 "
        ), "{text}");
        assert!(text.contains(
            "-drive if=pflash,format=qcow2,readonly=on,file=/fw/CODE.qcow2 -drive if=pflash,format=qcow2,file=/run/t/vars.img"
        ));
        let s3 = line(&machine(&["--s3"]), false, None);
        assert!(
            s3.starts_with("-machine q35 -global ICH9-LPC.disable_s3=0 "),
            "{s3}"
        );
    }

    #[test]
    fn install_cycle_disks_keep_their_serials_on_the_device() {
        let m = machine(&[
            "--disk",
            "/c/work.qcow2,id=tilo-installer-medium,serial=tilo-installer-medium,backing=/out/disk.qcow2",
            "--disk",
            "/c/target.qcow2,id=tilo-install-target,serial=tilo-install-target,create=40G",
            "--disk",
            "/v/validation.raw,id=tilo-validation,serial=tilo-validation,readonly",
        ]);
        let text = line(&m, false, None);
        assert!(text.contains("-drive file=/c/work.qcow2,format=qcow2,if=none,id=tilo-installer-medium -device virtio-blk-pci,drive=tilo-installer-medium,serial=tilo-installer-medium"), "{text}");
        assert!(text.contains(
            "-device virtio-blk-pci,drive=tilo-install-target,serial=tilo-install-target"
        ));
        assert!(text.contains("-drive file=/v/validation.raw,format=raw,if=none,id=tilo-validation,readonly=on -device virtio-blk-pci,drive=tilo-validation,serial=tilo-validation"));
        // QEMU rejects serial= on -drive.
        assert!(
            !text
                .split(" -device ")
                .any(|part| part.starts_with("-drive") && part.contains("serial="))
        );
        assert!(Disk::parse("/d,serial=has space", 0).is_err());
        assert!(Disk::parse("/d,colour=red", 0).is_err());
    }

    #[test]
    fn console_network_and_credentials() {
        let m = machine(&[
            "--console",
            "--ssh-port",
            "2299",
            "--forward",
            "tcp:127.0.0.1:3341-:3340",
            "--lan",
            "23901:23902:52:54:00:23:83:0a",
            "--lan-model",
            "e1000",
            "--virtio-port",
            "org.tilo.vm-install-proof=/run/t/install-proof.txt",
            "--audio-wav",
            "/run/t/out.wav",
            "--tpm",
        ]);
        let text = line(&m, false, Some("type=11,value=x".into()));
        assert!(text.contains("-nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:2299-:22,hostfwd=tcp:127.0.0.1:3341-:3340"), "{text}");
        assert!(text.contains("-netdev socket,id=tilolan,udp=127.0.0.1:23902,localaddr=127.0.0.1:23901 -device e1000,netdev=tilolan,mac=52:54:00:23:83:0a"));
        assert!(text.contains("-device virtio-serial-pci -chardev socket,id=tiloconsole,path=/run/t/console.sock,server=on,wait=off -device virtserialport,chardev=tiloconsole,name=org.tilo.console.0"));
        assert!(text.contains("-chardev file,id=tiloport0,path=/run/t/install-proof.txt -device virtserialport,chardev=tiloport0,name=org.tilo.vm-install-proof"));
        assert!(text.contains("-audiodev wav,id=tiloaudio,path=/run/t/out.wav -device virtio-sound-pci,audiodev=tiloaudio"));
        assert!(text.contains("-smbios type=11,value=x"));
        assert!(text.contains("-monitor unix:/run/t/mon.sock,server,nowait -serial file:/run/t/serial.log -pidfile /run/t/qemu.pid -name tilo"));
        assert!(text.ends_with("-chardev socket,id=chrtpm,path=/run/t/tpm/swtpm-ctrl.sock -tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-tis,tpmdev=tpm0"), "{text}");
        assert!(Lan::parse("23901:23902:52:54:00:23:83:0A", LanModel::Virtio).is_err());
        assert!(Lan::parse("1:23902:52:54:00:23:83:0a", LanModel::Virtio).is_err());
        let none = line(&machine(&["--no-net"]), false, None);
        assert!(none.contains("-nic none"));
    }

    #[test]
    fn touch_input_drops_the_keyboard_and_opens_qmp() {
        let text = line(&machine(&["--input", "touch"]), false, None);
        assert!(text.starts_with("-machine q35,i8042=off "), "{text}");
        assert!(text.contains(
            "-device virtio-multitouch-pci -qmp unix:/run/t/qmp.sock,server=on,wait=off"
        ));
        assert!(!text.contains("usb-tablet"));
    }

    #[test]
    fn a_serial_socket_keeps_the_serial_log() {
        let text = line(&machine(&["--serial-socket"]), false, None);
        assert!(text.contains(
            "-chardev socket,id=tiloserial,path=/run/t/serial.sock,server=on,wait=off,logfile=/run/t/serial.log"
        ), "{text}");
        assert!(
            text.contains("-serial chardev:tiloserial -pidfile"),
            "{text}"
        );
    }

    #[test]
    fn vnc_serves_the_local_display_on_a_socket() {
        let text = line(&machine(&["--vnc", "/run/t/vnc.sock"]), false, None);
        assert!(
            text.contains("-display none -vnc unix:/run/t/vnc.sock"),
            "the guest renders once, the VNC server serves those frames: {text}"
        );
        assert!(!line(&machine(&[]), false, None).contains("-vnc"));
    }

    #[test]
    fn displays_and_accelerators() {
        let gl = line(&machine(&["--display", "gl"]), false, None);
        assert!(gl.contains("-device virtio-vga-gl,xres=1280,yres=800 -display egl-headless,rendernode=/dev/dri/renderD129"));
        let window = line(
            &machine(&["--display", "window", "--resolution", "1920x1080"]),
            false,
            None,
        );
        assert!(window.contains("-device virtio-vga-gl,xres=1920,yres=1080 -display gtk,gl=on"));
        let none = line(&machine(&["--display", "none"]), false, None);
        assert!(none.contains("-vga none -display none"));
        let m = machine(&["--cpu-variant", "host-no-sve"]);
        assert_eq!(
            m.accel_args(|| true).unwrap().0.join(" "),
            "-enable-kvm -cpu host,sve=off"
        );
        let (tcg, warning) = m.accel_args(|| false).unwrap();
        assert_eq!(tcg.join(" "), "-accel tcg,thread=multi -cpu max");
        assert!(warning.is_some());
        let hvf = machine(&["--accel", "hvf"]).accel_args(|| false).unwrap().0;
        assert_eq!(hvf.join(" "), "-accel hvf -cpu host");
    }

    #[test]
    fn paths_with_commas_are_escaped_and_bad_settings_refused() {
        // A literal comma cannot be expressed in a --disk spec; it is refused.
        assert!(Disk::parse("/a,,b.qcow2", 0).is_err());
        assert_eq!(escape("/x,y"), "/x,,y");
        let args = Probe::parse_from(["probe", "--arch", "aarch64", "--s3"]).machine;
        assert!(Machine::try_from(&args).is_err());
        let args = Probe::parse_from(["probe", "--resolution", "wide"]).machine;
        assert!(Machine::try_from(&args).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn a_run_directory_whose_vm_runs_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let run = RunDir(dir.path().to_path_buf());
        claim(&run).unwrap();
        std::fs::write(run.pidfile(), std::process::id().to_string()).unwrap();
        assert_eq!(run.running(), Some(std::process::id()));
        assert!(claim(&run).unwrap_err().detail.contains("already running"));
        assert_eq!(status_command(dir.path()).data["running"], true);
    }

    #[test]
    fn a_later_flag_overrides_an_earlier_one() {
        use crate::cli::{Cli, Command, QemuCommand};
        let cli = Cli::try_parse_from([
            "tilo-vm", "qemu", "args", "--accel", "tcg", "--accel", "kvm", "--disk", "/a",
            "--disk", "/b",
        ])
        .unwrap();
        let Some(Command::Qemu(command)) = cli.command else {
            panic!("not a qemu command");
        };
        let QemuCommand::Args { machine } = *command else {
            panic!("not qemu args");
        };
        assert_eq!(machine.accel, Accel::Kvm);
        assert_eq!(machine.disks, ["/a", "/b"]);
    }

    #[test]
    fn the_root_key_travels_as_a_systemd_credential() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id.pub");
        std::fs::write(&key, "ssh-ed25519 AAAA comment, with comma\n").unwrap();
        let m = machine(&["--root-key", key.to_str().unwrap()]);
        let credential = m.credential().unwrap().unwrap();
        assert_eq!(
            credential,
            "type=11,value=io.systemd.credential.binary:ssh.authorized_keys.root=c3NoLWVkMjU1MTkgQUFBQSBjb21tZW50LCB3aXRoIGNvbW1hCg=="
        );
        std::fs::write(&key, "").unwrap();
        assert!(m.credential().is_err());
    }
}
