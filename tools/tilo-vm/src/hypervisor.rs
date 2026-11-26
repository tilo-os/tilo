//! Thin adapters over each host's native hypervisor tooling: create, start,
//! stop, destroy, the SSH endpoint, and optional debugging for when SSH is not
//! reachable. Adapters only build command lines; [`Runner`] executes them.

use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;

use crate::config::{DirectKernel, VmEntry};
use crate::error::CliError;
use crate::image;
use crate::sshkey;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandLine {
    pub program: String,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
}

impl CommandLine {
    pub fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            env: Vec::new(),
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn env(mut self, key: &str, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn display(&self) -> String {
        let quote = |s: &str| {
            if s.is_empty() || s.contains([' ', '"', '\'', '$', ';', '|']) {
                format!("'{}'", s.replace('\'', "'\\''"))
            } else {
                s.to_owned()
            }
        };
        let env = self.env.iter().fold(String::new(), |mut env, (k, v)| {
            env.push_str(k);
            env.push('=');
            env.push_str(&quote(v));
            env.push(' ');
            env
        });
        let args = self
            .args
            .iter()
            .map(|a| quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{env}{} {args}", self.program)
            .trim_end()
            .to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub success: bool,
    pub status: String,
    pub stdout: String,
    pub stderr: String,
}

pub trait Runner {
    fn output(&mut self, command: &CommandLine) -> Result<Output, CliError>;
    fn interactive(&mut self, command: &CommandLine) -> Result<bool, CliError>;
}

pub struct SystemRunner {
    pub cwd: Option<std::path::PathBuf>,
}

impl SystemRunner {
    fn command(&self, line: &CommandLine) -> std::process::Command {
        let mut command = std::process::Command::new(&line.program);
        if std::env::var_os("TILO_VM_BIN").is_none()
            && let Ok(exe) = std::env::current_exe()
        {
            command.env("TILO_VM_BIN", exe);
        }
        command
            .args(&line.args)
            .envs(line.env.iter().map(|(k, v)| (k, v)));
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
    }

    fn spawn_error(line: &CommandLine, error: &std::io::Error) -> CliError {
        if error.kind() == std::io::ErrorKind::NotFound {
            CliError::ToolMissing {
                tool: line.program.clone(),
                purpose: format!("run `{}`", line.display()),
            }
        } else {
            CliError::CommandFailed {
                command: line.display(),
                status: "not started".into(),
                stderr: error.to_string(),
            }
        }
    }
}

impl Runner for SystemRunner {
    fn output(&mut self, line: &CommandLine) -> Result<Output, CliError> {
        let output = self
            .command(line)
            .output()
            .map_err(|e| Self::spawn_error(line, &e))?;
        Ok(Output {
            success: output.status.success(),
            status: output.status.to_string(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn interactive(&mut self, line: &CommandLine) -> Result<bool, CliError> {
        let status = self
            .command(line)
            .status()
            .map_err(|e| Self::spawn_error(line, &e))?;
        Ok(status.success())
    }
}

pub fn run_all(runner: &mut dyn Runner, commands: &[CommandLine]) -> Result<(), CliError> {
    for command in commands {
        let output = runner.output(command)?;
        if !output.success {
            return Err(CliError::CommandFailed {
                command: command.display(),
                status: output.status,
                stderr: output.stderr.trim().to_owned(),
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SshEndpoint {
    pub user: String,
    pub host: String,
    pub port: u16,
}

impl SshEndpoint {
    /// `[user@]host[:port]`, defaulting to root on port 22.
    pub fn parse(text: &str) -> Option<Self> {
        let (user, rest) = text
            .rsplit_once('@')
            .map_or(("root", text), |(u, r)| (u, r));
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, port.parse().ok()?),
            _ => (rest, 22),
        };
        (!host.is_empty() && !user.is_empty()).then(|| Self {
            user: user.into(),
            host: host.into(),
            port,
        })
    }

    /// `user@host`, with `:port` when it is not 22: what [`Self::parse`] reads.
    pub fn ssh_spec(&self) -> String {
        if self.port == 22 {
            format!("{}@{}", self.user, self.host)
        } else {
            format!("{}@{}:{}", self.user, self.host, self.port)
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent flag per optional adapter capability"
)]
pub struct Capabilities {
    pub serial: bool,
    pub screenshot: bool,
    pub reset: bool,
    /// How the root SSH key reaches the guest as a systemd credential at
    /// creation. Without one, the key is installed once after the first boot
    /// (`tilo-vm guide platforms`).
    pub credentials: Option<&'static str>,
    pub control: Option<&'static str>,
    pub direct_kernel: bool,
    pub boot_esp: bool,
}

pub struct Vm<'a> {
    pub name: &'a str,
    pub entry: &'a VmEntry,
}

impl Vm<'_> {
    fn disk(&self) -> Result<&str, CliError> {
        self.entry.disk.as_deref().ok_or_else(|| CliError::Config {
            path: crate::config::registry_path().display().to_string(),
            detail: format!("VM '{}' has no `disk`", self.name),
        })
    }

    fn memory(&self) -> u32 {
        self.entry.memory_mb.unwrap_or_else(|| {
            if self.entry.hypervisor == "whpx" {
                3072
            } else {
                8192
            }
        })
    }

    #[must_use]
    pub fn memory_mb(&self) -> u32 {
        match (self.entry.memory_mb, self.entry.hypervisor.as_str()) {
            (None, "utm") => UTM_MEMORY_MB,
            _ => self.memory(),
        }
    }

    fn cpus(&self) -> u32 {
        self.entry.cpus.unwrap_or(4)
    }

    fn ssh_port(&self) -> u16 {
        self.entry.ssh_port.unwrap_or(2222)
    }
}

pub trait Hypervisor {
    fn name(&self) -> &'static str;
    fn create(&self, vm: &Vm<'_>, root_key: &str) -> Result<Vec<CommandLine>, CliError>;
    fn start(&self, vm: &Vm<'_>) -> Vec<CommandLine>;
    fn stop(&self, vm: &Vm<'_>) -> Vec<CommandLine>;
    fn destroy(&self, vm: &Vm<'_>) -> Vec<CommandLine>;
    fn ssh_endpoint(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Result<SshEndpoint, CliError>;

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn running(&self, _vm: &Vm<'_>, _runner: &mut dyn Runner) -> Option<bool> {
        None
    }
    fn serial(&self, _vm: &Vm<'_>) -> Option<CommandLine> {
        None
    }
    fn screenshot(&self, _vm: &Vm<'_>, _out: &Path) -> Option<CommandLine> {
        None
    }
    fn reset(&self, _vm: &Vm<'_>) -> Option<CommandLine> {
        None
    }
    fn control(&self, _vm: &Vm<'_>, _args: &[String]) -> Option<CommandLine> {
        None
    }
    fn redefine(
        &self,
        _vm: &Vm<'_>,
        _root_key: &str,
        _runner: &mut dyn Runner,
    ) -> Result<Vec<CommandLine>, CliError> {
        Err(CliError::Unsupported {
            what: format!("redefining a VM with the {} adapter", self.name()),
            alternative: "install the kernel in the VM instead: tilo-vm swap kernel".into(),
        })
    }
    fn power_off(&self, _vm: &Vm<'_>) -> Option<CommandLine> {
        None
    }
    fn wake(&self, _vm: &Vm<'_>) -> Option<Vec<CommandLine>> {
        None
    }
    fn suspended(&self, _vm: &Vm<'_>, _runner: &mut dyn Runner) -> Option<bool> {
        None
    }
    fn powers_on(&self) -> bool {
        true
    }
    fn ci_layers_from_host(&self) -> bool {
        false
    }
    fn branching(&self) -> Branching {
        Branching::Unsupported(format!(
            "the {} adapter cannot branch a VM; create each VM from the image instead: tilo-vm vm create <name>",
            self.name()
        ))
    }
    fn stopped(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Option<bool> {
        self.running(vm, runner).map(|running| !running)
    }
    fn assign_identity(&self, _vm: &Vm<'_>) -> Vec<CommandLine> {
        Vec::new()
    }
    fn query_uuid(&self, _vm: &Vm<'_>, _runner: &mut dyn Runner) -> Option<String> {
        None
    }
    /// Whether the VM is on a network the host's other VMs are on ([`crate::lan`]): `Ok` when
    /// it is, else why not.
    fn host_network(&self, _entry: &VmEntry) -> Result<(), String> {
        Err(format!(
            "the {} adapter's VMs reach out from this host but not each other",
            self.name()
        ))
    }
    /// Whether this adapter puts its VMs on [`crate::lan`]'s network.
    fn joins_lan(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Branching {
    Backing,
    Clone,
    Unsupported(String),
}

/// A new VM's identity: a random UUID, and a random MAC in QEMU's
/// `52:54:00` range.
pub fn new_identity() -> (String, String) {
    let [a, b, c, ..] = *uuid::Uuid::new_v4().as_bytes();
    (
        uuid::Uuid::new_v4().to_string(),
        format!("52:54:00:{a:02x}:{b:02x}:{c:02x}"),
    )
}

pub fn adapter(name: &str) -> Result<Box<dyn Hypervisor>, CliError> {
    match name {
        "libvirt" => Ok(Box::new(Libvirt)),
        "qemu" => Ok(Box::new(Qemu)),
        "utm" => Ok(Box::new(Utm)),
        "hyperv" => Ok(Box::new(HyperV)),
        "whpx" => Ok(Box::new(Whpx)),
        DEVICE => Ok(Box::new(Device)),
        other => Err(CliError::Unsupported {
            what: format!("hypervisor '{other}'"),
            alternative:
                "set `hypervisor` to libvirt, qemu, utm, hyperv, whpx or device in vms.toml".into(),
        }),
    }
}

fn configured_endpoint(vm: &Vm<'_>, default_host: &str, port: u16) -> SshEndpoint {
    vm.entry
        .ssh
        .as_deref()
        .and_then(SshEndpoint::parse)
        .unwrap_or_else(|| SshEndpoint {
            user: "root".into(),
            host: default_host.into(),
            port,
        })
}

pub fn overlay_disk(vm: &Vm<'_>) -> Result<Option<CommandLine>, CliError> {
    let disk = vm.disk()?;
    let qemu_img = vm.entry.qemu_img.as_deref().unwrap_or("qemu-img");
    let format = image::Format::from_entry(vm.entry.image_format.as_deref()).name();
    Ok(vm.entry.image.as_deref().map(|image| {
        CommandLine::new(
            qemu_img,
            &["create", "-f", "qcow2", "-F", format, "-b", image, disk],
        )
    }))
}

#[must_use]
pub fn vm_files(disk: &str) -> Vec<String> {
    let mut files = vec![
        disk.to_owned(),
        domain_xml_path(disk),
        crate::sound::wav_path(disk),
    ];
    files.extend(
        ["vars.fd", "pid", "ttyS0.log", "uart.log"]
            .iter()
            .map(|suffix| whpx_file(disk, suffix)),
    );
    files
}

pub const DEVICE: &str = "device";

pub struct Device;

impl Hypervisor for Device {
    fn name(&self) -> &'static str {
        DEVICE
    }

    fn create(&self, vm: &Vm<'_>, _root_key: &str) -> Result<Vec<CommandLine>, CliError> {
        Err(CliError::Unsupported {
            what: format!("creating device {}", vm.name),
            alternative: format!(
                "register a running device instead: tilo-vm vm add {} --ssh root@<address>",
                vm.name
            ),
        })
    }

    fn start(&self, _vm: &Vm<'_>) -> Vec<CommandLine> {
        Vec::new()
    }

    fn stop(&self, _vm: &Vm<'_>) -> Vec<CommandLine> {
        Vec::new()
    }

    fn destroy(&self, _vm: &Vm<'_>) -> Vec<CommandLine> {
        Vec::new()
    }

    fn ssh_endpoint(&self, vm: &Vm<'_>, _runner: &mut dyn Runner) -> Result<SshEndpoint, CliError> {
        vm.entry
            .ssh
            .as_deref()
            .and_then(SshEndpoint::parse)
            .ok_or_else(|| CliError::Config {
                path: crate::config::registry_path().display().to_string(),
                detail: format!(
                    "device '{}' has no `ssh` address; register it again: tilo-vm vm add {} --ssh <user>@<address>",
                    vm.name, vm.name
                ),
            })
    }

    fn powers_on(&self) -> bool {
        false
    }

    fn ci_layers_from_host(&self) -> bool {
        true
    }

    fn branching(&self) -> Branching {
        Branching::Unsupported(
            "a device is a real machine with its own disk; install another device instead (tilo-vm guide devices)".into(),
        )
    }
}

pub struct Libvirt;

impl Libvirt {
    fn define(vm: &Vm<'_>, root_key: &str, uuid: Option<&str>) -> Result<CommandLine, CliError> {
        let xml_path = domain_xml_path(vm.disk()?);
        std::fs::write(&xml_path, domain_xml_with_uuid(vm, root_key, uuid)?)
            .map_err(CliError::io(format!("write {xml_path}")))?;
        Ok(Self::virsh(vm, &["define", &xml_path]))
    }

    fn virsh(vm: &Vm<'_>, args: &[&str]) -> CommandLine {
        CommandLine::new(
            "virsh",
            &[
                "--connect",
                vm.entry.uri.as_deref().unwrap_or("qemu:///session"),
            ],
        )
        .args_from(args)
    }
}

/// The systemd credentials a VM's device daemon reads: `tilo.port`, the UDP port it binds, which
/// the hypervisor forwards a host port to; and, until the VM has joined, `tilo.ticket`, the
/// `tilo device add` ticket it spends at its first boot to join this machine's person as one of
/// their devices.
pub fn device_credentials(ticket: Option<&str>) -> Vec<(&'static str, String)> {
    let mut credentials = vec![("tilo.port", crate::config::DAEMON_PORT.to_string())];
    credentials.extend(ticket.map(|ticket| ("tilo.ticket", ticket.to_owned())));
    credentials
}

/// A credential as an SMBIOS type 11 OEM string.
pub fn credential_smbios((name, value): &(&str, String)) -> String {
    format!("io.systemd.credential:{name}={value}")
}

/// QEMU's arguments that pass the device credentials as SMBIOS type 11 OEM strings.
pub fn smbios_args(ticket: Option<&str>) -> Vec<String> {
    device_credentials(ticket)
        .iter()
        .flat_map(|credential| {
            [
                "-smbios".to_owned(),
                format!("type=11,value={}", credential_smbios(credential)),
            ]
        })
        .collect()
}

/// A credential on a kernel command line, for a VM booted from its kernel directly.
pub fn credential_kernel((name, value): &(&str, String)) -> String {
    format!("systemd.set_credential={name}:{value}")
}

pub fn domain_xml_path(disk: &str) -> String {
    format!("{disk}.xml")
}

/// The libvirt domain for `vm`, defined with `virsh define`. That needs only
/// libvirt's QEMU driver: no storage or network driver, no virt-install.
///
/// UEFI without Secure Boot (local dev images may carry an unsigned kernel),
/// user-mode networking through passt with the SSH port forwarded, SPICE and
/// virtio video for virt-manager, and `root_key` as the SMBIOS type 11
/// credential systemd turns into root's `authorized_keys`. KVM with the
/// host's CPU, or TCG with the emulator's widest CPU when `emulated`.
pub fn domain_xml(vm: &Vm<'_>, root_key: &str) -> Result<String, CliError> {
    domain_xml_with_uuid(vm, root_key, None)
}

/// [`domain_xml`] for a domain that already exists: libvirt redefines a
/// domain only under its own UUID.
#[expect(
    clippy::too_many_lines,
    reason = "one domain document assembled from the entry, field by field"
)]
pub fn domain_xml_with_uuid(
    vm: &Vm<'_>,
    root_key: &str,
    uuid: Option<&str>,
) -> Result<String, CliError> {
    let arch = vm.entry.arch.as_deref().unwrap_or(std::env::consts::ARCH);
    let (machine, features) = match arch {
        "x86_64" => ("q35", "<acpi/><apic/>"),
        _ => ("virt", "<acpi/>"),
    };
    let (domain_type, cpu) = if vm.entry.emulated {
        ("qemu", "maximum")
    } else {
        ("kvm", "host-passthrough")
    };
    let e = quick_xml::escape::escape;
    let uuid = uuid.or(vm.entry.uuid.as_deref());
    let lan = crate::lan::mac(vm.entry);
    // libvirt otherwise picks the machine's default render node, which on a
    // laptop with a discrete NVIDIA card is that card: its EGL stack
    // initialises no context for virglrenderer, the guest gets no GL, and a
    // screenshot taken from outside the guest has no surface. Name the node
    // the boot reports instead.
    let gl = if vm.entry.gl {
        let node = match vm.entry.render_node.as_deref() {
            Some(node) => node.to_owned(),
            None => crate::qemu::resolve_rendernode(None)?.path,
        };
        format!(
            "\n    <graphics type='egl-headless'><gl enable='yes' rendernode='{}'/></graphics>",
            quick_xml::escape::escape(node.as_str())
        )
    } else {
        String::new()
    };
    // QEMU's wav backend writes what the guest plays to a file beside the
    // disk, so nothing reaches this computer's own audio and `vm sound` can
    // measure it. virtio-snd: the guest kernel's driver needs no codec quirks.
    let sound = vm.entry.disk.as_deref().filter(|_| vm.entry.sound).map_or_else(String::new, |disk| {
        let wav = crate::sound::wav_path(disk);
        format!(
            "\n    <sound model='virtio'><audio id='1'/></sound>\n    <audio id='1' type='file' path='{}'/>",
            quick_xml::escape::escape(wav.as_str())
        )
    });
    let venus = vm.entry.gl && vm.entry.vulkan;
    let qemu_namespace = lan.is_some() || venus;
    Ok(format!(
        "<domain type='{domain_type}'{namespace}>
  <name>{name}</name>{uuid}
  <memory unit='MiB'>{memory}</memory>{memory_backing}
  <vcpu>{cpus}</vcpu>
  <os firmware='efi'>
    <type arch='{arch}' machine='{machine}'>hvm</type>
    <firmware>
      <feature enabled='no' name='secure-boot'/>
      <feature enabled='no' name='enrolled-keys'/>
    </firmware>
    <smbios mode='sysinfo'/>{direct}
  </os>
  <sysinfo type='smbios'>{system}
    <oemStrings>
      <entry>{credential}</entry>{device}
    </oemStrings>
  </sysinfo>
  <features>{features}</features>
  <cpu mode='{cpu}'/>
  <devices>
    <disk type='file' device='disk'>
      <driver name='qemu' type='qcow2' discard='unmap'/>
      <source file='{disk}'/>
      <target dev='vda' bus='virtio'/>{disk_boot}
    </disk>{boot_esp}
    <interface type='user'>{mac}
      <backend type='passt'/>
      <model type='virtio'/>
      <portForward proto='tcp'><range start='{port}' to='22'/></portForward>
      <portForward proto='udp'><range start='{port}' to='{daemon}'/></portForward>
    </interface>
    <serial type='pty'/>
    <console type='pty'/>
    <controller type='usb' model='qemu-xhci'/>
    <input type='tablet' bus='usb'/>
    <input type='keyboard' bus='usb'/>
    <graphics type='spice' autoport='yes'/>{gl}
    <video>{video}</video>{sound}
    <rng model='virtio'><backend model='random'>/dev/urandom</backend></rng>
  </devices>{lan}{venus}
</domain>
",
        namespace = if qemu_namespace { crate::lan::QEMU_NAMESPACE } else { "" },
        lan = lan.map_or_else(String::new, crate::lan::domain_commandline),
        // Venus (Vulkan over virtio-gpu) maps host GPU memory into the guest
        // as blob resources; libvirt has no attribute for venus or hostmem,
        // so the video device gets them as QEMU properties.
        venus = if venus { VENUS_OVERRIDE } else { "" },
        memory_backing = if venus {
            "\n  <memoryBacking><source type='memfd'/><access mode='shared'/></memoryBacking>"
        } else {
            ""
        },
        name = e(vm.name),
        uuid = uuid.map_or_else(String::new, |u| format!("\n  <uuid>{}</uuid>", e(u))),
        system = uuid.map_or_else(String::new, |u| format!(
            "\n    <system>\n      <entry name='uuid'>{}</entry>\n    </system>",
            e(u)
        )),
        mac = vm.entry.nic_mac.as_deref().map_or_else(String::new, |m| format!(
            "\n      <mac address='{}'/>",
            e(m)
        )),
        memory = vm.memory(),
        cpus = vm.cpus(),
        arch = e(arch),
        credential = e(&sshkey::smbios_credential(root_key)),
        device = device_credentials(vm.entry.ticket.as_deref()).iter().fold(
            String::new(),
            |mut entries, credential| {
                let _ = write!(
                    entries,
                    "\n      <entry>{}</entry>",
                    quick_xml::escape::escape(credential_smbios(credential))
                );
                entries
            }
        ),
        daemon = crate::config::DAEMON_PORT,
        disk = e(vm.disk()?),
        port = vm.ssh_port(),
        video = video_model(vm.entry),
        gl = gl,
        disk_boot = if vm.entry.boot_esp.is_some() {
            "\n      <boot order='2'/>"
        } else {
            ""
        },
        boot_esp = vm.entry.boot_esp.as_deref().map_or_else(String::new, |esp| format!(
            "\n    <disk type='file' device='disk'>\n      <driver name='qemu' type='raw'/>\n      <source file='{}'/>\n      <target dev='vdb' bus='virtio'/>\n      <readonly/>\n      <boot order='1'/>\n    </disk>",
            e(esp)
        )),
        direct = vm
            .entry
            .direct_kernel
            .as_ref()
            .map_or_else(String::new, |d| format!(
                "\n    <kernel>{}</kernel>\n    <initrd>{}</initrd>\n    <cmdline>{}</cmdline>",
                e(&d.kernel),
                e(&d.initrd),
                e(&d.cmdline)
            )),
    ))
}

/// 4 GiB: the guest-visible window of host GPU memory Venus maps blobs
/// into, as `qemu::VENUS_HOSTMEM` for the QEMU adapter.
const VENUS_OVERRIDE: &str = "
  <qemu:override>
    <qemu:device alias='video0'>
      <qemu:frontend>
        <qemu:property name='venus' type='bool' value='true'/>
        <qemu:property name='hostmem' type='unsigned' value='4294967296'/>
      </qemu:frontend>
    </qemu:device>
  </qemu:override>";

/// Venus starts virglrenderer's render server as a child of QEMU, which
/// libvirt's default seccomp sandbox (`spawn=deny`) forbids: the guest then
/// sees no Vulkan device at all. Read the setting from the qemu.conf of the
/// libvirt daemon the VM uses and refuse with the fix while it is on.
///
/// # Errors
/// `seccomp_sandbox` is not 0 in that qemu.conf.
pub fn venus_sandbox_check(uri: &str) -> Result<(), CliError> {
    let path = if uri.contains("/session") {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))
            .unwrap_or_default()
            .join("libvirt/qemu.conf")
    } else {
        std::path::PathBuf::from("/etc/libvirt/qemu.conf")
    };
    venus_sandbox_check_at(&path, uri)
}

fn venus_sandbox_check_at(path: &Path, uri: &str) -> Result<(), CliError> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    if seccomp_sandbox_off(&text) {
        return Ok(());
    }
    Err(CliError::Unsupported {
        what: format!(
            "--vulkan with libvirt: {} does not set seccomp_sandbox = 0, so libvirt starts QEMU with -sandbox spawn=deny and QEMU may not start virglrenderer's Vulkan render server",
            path.display()
        ),
        alternative: format!(
            "add the line `seccomp_sandbox = 0` to {} and let libvirt's daemon restart (it reads the file when it starts: stop every VM of {uri}, or `systemctl --user restart virtqemud` once none is up), or use --hypervisor qemu, which starts QEMU without that sandbox",
            path.display()
        ),
    })
}

fn seccomp_sandbox_off(qemu_conf: &str) -> bool {
    qemu_conf.lines().any(|line| {
        let line = line.split('#').next().unwrap_or_default();
        line.split_once('=')
            .is_some_and(|(key, value)| key.trim() == "seccomp_sandbox" && value.trim() == "0")
    })
}

fn video_model(entry: &VmEntry) -> String {
    let heads = entry
        .heads
        .filter(|&n| n > 1)
        .map_or_else(String::new, |n| format!(" heads='{n}'"));
    let accel = if entry.gl {
        "<acceleration accel3d='yes'/>"
    } else {
        ""
    };
    let blob = if entry.gl && entry.vulkan {
        " blob='on'"
    } else {
        ""
    };
    let resolution = entry
        .resolution
        .as_deref()
        .and_then(|r| r.split_once('x'))
        .map_or_else(String::new, |(x, y)| {
            format!(
                "<resolution x='{}' y='{}'/>",
                quick_xml::escape::escape(x),
                quick_xml::escape::escape(y)
            )
        });
    if accel.is_empty() && resolution.is_empty() {
        format!("<model type='virtio'{heads}{blob}/>")
    } else {
        format!("<model type='virtio'{heads}{blob}>{accel}{resolution}</model>")
    }
}

impl CommandLine {
    #[must_use]
    fn args_from(mut self, args: &[&str]) -> Self {
        self.args.extend(args.iter().map(|a| (*a).to_owned()));
        self
    }
}

impl Hypervisor for Libvirt {
    fn name(&self) -> &'static str {
        "libvirt"
    }

    fn create(&self, vm: &Vm<'_>, root_key: &str) -> Result<Vec<CommandLine>, CliError> {
        if vm.entry.gl && vm.entry.vulkan {
            venus_sandbox_check(vm.entry.uri.as_deref().unwrap_or("qemu:///session"))?;
        }
        let mut commands: Vec<CommandLine> = overlay_disk(vm)?.into_iter().collect();
        commands.push(Self::define(vm, root_key, None)?);
        Ok(commands)
    }

    fn redefine(
        &self,
        vm: &Vm<'_>,
        root_key: &str,
        runner: &mut dyn Runner,
    ) -> Result<Vec<CommandLine>, CliError> {
        let uuid = vm.entry.uuid.clone().or_else(|| {
            runner
                .output(&Self::virsh(vm, &["domuuid", vm.name]))
                .ok()
                .filter(|o| o.success)
                .map(|o| o.stdout.trim().to_owned())
                .filter(|u| !u.is_empty())
        });
        Ok(vec![Self::define(vm, root_key, uuid.as_deref())?])
    }

    fn power_off(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(Self::virsh(vm, &["destroy", vm.name]))
    }

    fn start(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![Self::virsh(vm, &["start", vm.name])]
    }

    fn stop(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![Self::virsh(vm, &["shutdown", vm.name])]
    }

    fn destroy(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![Self::virsh(vm, &["undefine", vm.name, "--nvram"])]
    }

    fn ssh_endpoint(&self, vm: &Vm<'_>, _runner: &mut dyn Runner) -> Result<SshEndpoint, CliError> {
        Ok(configured_endpoint(vm, "127.0.0.1", vm.ssh_port()))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            serial: true,
            screenshot: true,
            reset: true,
            credentials: Some("SMBIOS type 11 OEM string"),
            control: Some("virsh"),
            direct_kernel: true,
            boot_esp: true,
        }
    }

    fn running(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Option<bool> {
        // Names only, so the answer does not depend on virsh's locale.
        let output = runner
            .output(&Self::virsh(vm, &["list", "--name", "--state-running"]))
            .ok()
            .filter(|o| o.success)?;
        Some(output.stdout.lines().any(|l| l.trim() == vm.name))
    }

    fn wake(&self, vm: &Vm<'_>) -> Option<Vec<CommandLine>> {
        Some(vec![Self::virsh(vm, &["dompmwakeup", vm.name])])
    }

    fn suspended(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Option<bool> {
        let output = runner
            .output(&Self::virsh(vm, &["domstate", vm.name]).env("LC_ALL", "C"))
            .ok()
            .filter(|o| o.success)?;
        Some(output.stdout.trim() == "pmsuspended")
    }

    fn branching(&self) -> Branching {
        Branching::Backing
    }

    fn host_network(&self, entry: &VmEntry) -> Result<(), String> {
        crate::lan::mac(entry).map(|_| ()).ok_or_else(|| {
            "its NIC on tilo-vm's host network is off (`lan = \"off\"` in vms.toml)".to_owned()
        })
    }

    fn joins_lan(&self) -> bool {
        true
    }

    fn stopped(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Option<bool> {
        let output = runner
            .output(&Self::virsh(vm, &["domstate", vm.name]).env("LC_ALL", "C"))
            .ok()
            .filter(|o| o.success)?;
        Some(output.stdout.trim() == "shut off")
    }

    fn serial(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(Self::virsh(vm, &["console", vm.name]))
    }

    fn screenshot(&self, vm: &Vm<'_>, out: &Path) -> Option<CommandLine> {
        Some(Self::virsh(vm, &["screenshot", vm.name]).arg(out.display().to_string()))
    }

    fn reset(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(Self::virsh(vm, &["reset", vm.name]))
    }

    fn control(&self, vm: &Vm<'_>, args: &[String]) -> Option<CommandLine> {
        let mut command = Self::virsh(vm, &[]);
        command.args.extend(args.iter().cloned());
        Some(command)
    }
}

pub struct Qemu;

impl Qemu {
    fn script(vm: &Vm<'_>, args: &[&str]) -> CommandLine {
        // The preflight checked QEMU, its modules and firmware on this computer, so
        // the VM starts with those. vm.sh would otherwise enter its default
        // distrobox, whose older QEMU may lack the GL display module and crashed at
        // device setup instead of saying so.
        CommandLine::new("bash", &["os/dev/vm.sh"])
            .args_from(args)
            .env("TILO_DISTROBOX", "")
            .env("TILO_SSH_PORT", vm.ssh_port().to_string())
    }
}

impl Hypervisor for Qemu {
    fn name(&self) -> &'static str {
        "qemu"
    }

    fn create(&self, vm: &Vm<'_>, _root_key: &str) -> Result<Vec<CommandLine>, CliError> {
        Ok(overlay_disk(vm)?.into_iter().collect())
    }

    fn start(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        let disk = vm.entry.disk.clone().unwrap_or_default();
        let display = match (vm.entry.gl, vm.entry.vulkan) {
            (true, true) => "--vulkan",
            (true, false) => "--gl",
            _ => "--2d",
        };
        let args = ["boot", display, "--disk", disk.as_str()];
        let mut command = Self::script(vm, &args)
            .env("TILO_MEM", vm.memory().to_string())
            .env("TILO_SMP", vm.cpus().to_string());
        // vm.sh takes the render node from the environment (os/dev/_common.sh), not as an
        // argument of `boot`.
        if let (true, Some(node)) = (vm.entry.gl, vm.entry.render_node.as_deref()) {
            command = command.env("TILO_RENDERNODE", node);
        }
        if vm.entry.sound {
            command = command.env("TILO_VM_AUDIO_WAV", crate::sound::wav_path(&disk));
        }
        if let Some(key) = &vm.entry.key {
            command = command.env(
                "TILO_VM_ROOT_KEY_FILE",
                sshkey::public_path(Path::new(key)).display().to_string(),
            );
        }
        if let Some(ticket) = &vm.entry.ticket {
            command = command.env("TILO_VM_TICKET", ticket);
        }
        if vm.entry.emulated {
            command = command.env("TILO_ACCEL", "tcg");
        }
        if let Some(arch) = &vm.entry.arch {
            command = command.env("TILO_VM_ARCH", arch);
        }
        if let Some((width, height)) = vm
            .entry
            .resolution
            .as_deref()
            .and_then(|r| r.split_once('x'))
            && let (Ok(width), Ok(height)) = (width.parse::<u32>(), height.parse::<u32>())
            && width > 0
            && height > 0
        {
            command = command
                .env("TILO_XRES", width.to_string())
                .env("TILO_YRES", height.to_string());
        }
        vec![command]
    }

    fn stop(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![Self::script(vm, &["cmd", "system_powerdown"])]
    }

    fn destroy(&self, _vm: &Vm<'_>) -> Vec<CommandLine> {
        Vec::new()
    }

    fn power_off(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(Self::script(vm, &["stop"]))
    }

    fn ssh_endpoint(&self, vm: &Vm<'_>, _runner: &mut dyn Runner) -> Result<SshEndpoint, CliError> {
        Ok(configured_endpoint(vm, "127.0.0.1", vm.ssh_port()))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            serial: true,
            screenshot: true,
            reset: true,
            credentials: Some("SMBIOS type 11 OEM string"),
            control: Some("QEMU monitor (HMP)"),
            direct_kernel: false,
            boot_esp: false,
        }
    }

    fn running(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Option<bool> {
        let output = runner
            .output(&Self::script(vm, &["status"]))
            .ok()
            .filter(|output| output.success)?;
        let status = output.stdout.trim();
        if status == "stopped" {
            Some(false)
        } else if status.starts_with("running (pid ") && status.ends_with(')') {
            Some(true)
        } else {
            None
        }
    }

    fn branching(&self) -> Branching {
        Branching::Unsupported(
            "the qemu adapter runs CI and scenario VMs through os/dev/vm.sh, which gives a VM no UUID of its own, so a branch could not tell it was branched; set up the box with libvirt: tilo-vm vm create <name> --hypervisor libvirt".into(),
        )
    }

    fn serial(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(Self::script(vm, &["serial"]))
    }

    fn screenshot(&self, vm: &Vm<'_>, out: &Path) -> Option<CommandLine> {
        // TILO_RUN also selects the running VM's monitor. Keep it unchanged:
        // capture there, then move the image to the requested evidence path.
        // Pass the destination as an argument, never interpolate it into shell.
        Some(
            CommandLine::new(
                "bash",
                &[
                    "-c",
                    "image=$(bash os/dev/vm.sh shot \"tilo-vm-shot-$$\") && mv -- \"$image\" \"$1\"",
                    "tilo-vm-shot",
                ],
            )
            .arg(out.display().to_string())
            .env("TILO_DISTROBOX", "")
            .env("TILO_SSH_PORT", vm.ssh_port().to_string()),
        )
    }

    fn reset(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(Self::script(vm, &["cmd", "system_reset"]))
    }

    fn control(&self, vm: &Vm<'_>, args: &[String]) -> Option<CommandLine> {
        Some(Self::script(vm, &["cmd"]).arg(args.join(" ")))
    }
}

pub struct Utm;

fn utm_quote(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn utm_script(lines: &[String]) -> CommandLine {
    let mut command = CommandLine::new("osascript", &["-e", "tell application \"UTM\""]);
    for line in lines {
        command = command.arg("-e").arg(line.as_str());
    }
    command.arg("-e").arg("end tell")
}

pub const UTM_MEMORY_MB: u32 = 16384;
pub const UTM_CPUS: u32 = 8;

pub fn utm_import_template(
    runner: &mut dyn Runner,
    (name, disk): (&str, &str),
    (memory_mb, cpus): (u32, u32),
) -> Result<(), CliError> {
    let quote = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    let lines = [
        format!("set diskFile to POSIX file \"{}\"", quote(disk)),
        "tell application \"UTM\"".to_owned(),
        format!(
            "make new virtual machine with properties {{backend:qemu, configuration:{{name:\"{}\", architecture:\"aarch64\", memory:{memory_mb}, cpu cores:{cpus}, hypervisor:true, uefi:true, drives:{{{{interface:virtio, source:diskFile}}}}}}}}",
            quote(name)
        ),
        "end tell".to_owned(),
    ];
    let mut args = Vec::new();
    for line in &lines {
        args.push("-e");
        args.push(line.as_str());
    }
    let command = CommandLine::new("osascript", &args);
    let output = runner.output(&command)?;
    if output.success {
        return Ok(());
    }
    Err(CliError::Unreachable {
        vm: name.into(),
        detail: format!("UTM did not import {disk}: {}", output.stderr.trim()),
        debug: vec!["open UTM once and allow this terminal to control it (System Settings > Privacy & Security > Automation)".into()],
    })
}

/// Where macOS's DHCP server (vmnet's shared network, UTM's default) records
/// the addresses it leased.
const UTM_LEASES: &str = "/var/db/dhcpd_leases";

fn utm_leased_address(vm: &Vm<'_>, runner: &mut dyn Runner) -> Option<String> {
    let configuration = format!(
        "set c to configuration of virtual machine \"{}\"",
        vm.name.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let output = runner
        .output(&CommandLine::new(
            "osascript",
            &[
                "-e",
                "tell application \"UTM\"",
                "-e",
                &configuration,
                "-e",
                "return address of item 1 of (network interfaces of c)",
                "-e",
                "end tell",
            ],
        ))
        .ok()
        .filter(|output| output.success)?;
    let leases = std::fs::read_to_string(UTM_LEASES).ok()?;
    leased_address(&leases, output.stdout.trim())
}

fn leased_address(leases: &str, mac: &str) -> Option<String> {
    let octets = |text: &str| -> Option<Vec<u8>> {
        text.split(':')
            .map(|octet| u8::from_str_radix(octet, 16).ok())
            .collect()
    };
    let wanted = octets(mac).filter(|octets| octets.len() == 6)?;
    let mut found = None;
    for entry in leases.split('}') {
        let field = |name: &str| {
            entry.lines().find_map(|line| {
                line.trim()
                    .strip_prefix(name)?
                    .strip_prefix('=')
                    .map(str::trim)
            })
        };
        let hardware = field("hw_address")
            .and_then(|value| value.split_once(','))
            .map(|(_, mac)| mac);
        if hardware.and_then(octets).as_ref() == Some(&wanted)
            && let Some(address) = field("ip_address")
        {
            found.get_or_insert_with(|| address.to_owned());
        }
    }
    found
}

impl Hypervisor for Utm {
    fn name(&self) -> &'static str {
        "utm"
    }

    fn create(&self, vm: &Vm<'_>, _root_key: &str) -> Result<Vec<CommandLine>, CliError> {
        // No credential channel: Apple Virtualization has no SMBIOS OEM
        // strings or fw_cfg, and utmctl cannot set QEMU arguments.
        // utmctl cannot import a disk: `vm create --image` imports it once
        // over UTM's AppleScript as the template VM ([`utm_import_template`]),
        // and each dev VM is a clone of it.
        let template = vm
            .entry
            .template
            .as_deref()
            .ok_or_else(|| CliError::Config {
                path: crate::config::registry_path().display().to_string(),
                detail: format!(
                    "VM '{}' needs `template`, the UTM VM imported from the dev image",
                    vm.name
                ),
            })?;
        Ok(vec![CommandLine::new(
            "utmctl",
            &["clone", template, "--name", vm.name],
        )])
    }

    fn start(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![CommandLine::new("utmctl", &["start", vm.name])]
    }

    fn stop(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![CommandLine::new("utmctl", &["stop", vm.name, "--request"])]
    }

    fn destroy(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![CommandLine::new("utmctl", &["delete", vm.name])]
    }

    fn power_off(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(CommandLine::new("utmctl", &["stop", vm.name, "--kill"]))
    }

    fn ssh_endpoint(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Result<SshEndpoint, CliError> {
        if let Some(endpoint) = vm.entry.ssh.as_deref().and_then(SshEndpoint::parse) {
            return Ok(endpoint);
        }
        let command = CommandLine::new("utmctl", &["ip-address", vm.name]);
        let output = runner.output(&command)?;
        let host = output
            .stdout
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.contains(':'))
            .filter(|_| output.success)
            .map(str::to_owned)
            .or_else(|| utm_leased_address(vm, runner));
        match host {
            Some(host) => Ok(SshEndpoint {
                user: "root".into(),
                host,
                port: 22,
            }),
            None => Err(CliError::Unreachable {
                vm: vm.name.into(),
                detail: format!(
                    "`{}` reported no IPv4 address, and {UTM_LEASES} holds none for the VM's network interface",
                    command.display()
                ),
                debug: vec![format!("tilo-vm vm serial {}", vm.name)],
            }),
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            serial: true,
            screenshot: false,
            reset: false,
            credentials: None,
            control: Some("utmctl"),
            direct_kernel: false,
            boot_esp: false,
        }
    }

    fn serial(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(CommandLine::new("utmctl", &["attach", vm.name]))
    }

    fn branching(&self) -> Branching {
        Branching::Clone
    }

    fn host_network(&self, _entry: &VmEntry) -> Result<(), String> {
        // UTM's default shared network (vmnet) puts its VMs on one subnet of the host.
        Ok(())
    }

    fn stopped(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Option<bool> {
        let output = runner
            .output(&CommandLine::new("utmctl", &["status", vm.name]))
            .ok()
            .filter(|o| o.success)?;
        match output.stdout.trim() {
            "stopped" => Some(true),
            "starting" | "started" | "pausing" | "paused" | "resuming" | "stopping" => Some(false),
            _ => None,
        }
    }

    fn assign_identity(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        let Some(mac) = vm.entry.nic_mac.as_deref() else {
            return Vec::new();
        };
        vec![utm_script(&[
            format!("set m to virtual machine \"{}\"", utm_quote(vm.name)),
            "set c to configuration of m".into(),
            "set n to network interfaces of c".into(),
            "set i to item 1 of n".into(),
            format!("set address of i to \"{}\"", utm_quote(mac)),
            "set item 1 of n to i".into(),
            "set network interfaces of c to n".into(),
            "update configuration of m with c".into(),
        ])]
    }

    fn query_uuid(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Option<String> {
        let script = utm_script(&[format!(
            "return id of virtual machine \"{}\"",
            utm_quote(vm.name)
        )]);
        let output = runner.output(&script).ok().filter(|o| o.success)?;
        let id = output.stdout.trim().to_ascii_lowercase();
        uuid::Uuid::parse_str(&id).is_ok().then_some(id)
    }

    fn control(&self, _vm: &Vm<'_>, args: &[String]) -> Option<CommandLine> {
        let mut command = CommandLine::new("utmctl", &[]);
        command.args.extend(args.iter().cloned());
        Some(command)
    }
}

/// Windows ARM64 Home has WHPX even though it has no Hyper-V management
/// module. QEMU is launched detached; the published qcow2 stays read-only.
pub struct Whpx;

fn whpx_file(disk: &str, suffix: &str) -> String {
    format!("{disk}.{suffix}")
}

fn win_arg(text: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes: usize = 0;
    for c in text.chars() {
        match c {
            '\\' => slashes = slashes.saturating_add(1),
            '"' => {
                result.push_str(&"\\".repeat(slashes.saturating_mul(2).saturating_add(1)));
                result.push('"');
                slashes = 0;
            }
            _ => {
                result.push_str(&"\\".repeat(slashes));
                slashes = 0;
                result.push(c);
            }
        }
    }
    result.push_str(&"\\".repeat(slashes.saturating_mul(2)));
    result.push('"');
    result
}

impl Whpx {
    fn public_key(vm: &Vm<'_>) -> Option<String> {
        let key = vm.entry.key.as_deref()?;
        std::fs::read_to_string(format!("{key}.pub"))
            .ok()
            .map(|public| public.trim().to_owned())
    }

    fn pid(vm: &Vm<'_>) -> String {
        whpx_file(vm.entry.disk.as_deref().unwrap_or_default(), "pid")
    }

    fn qmp_port(vm: &Vm<'_>) -> u16 {
        20_000_u16.saturating_add(vm.ssh_port() % 40_000)
    }

    fn uuid_args(vm: &Vm<'_>) -> Vec<String> {
        vm.entry
            .uuid
            .iter()
            .flat_map(|u| ["-uuid".to_owned(), u.clone()])
            .collect()
    }

    fn net_device(vm: &Vm<'_>) -> String {
        vm.entry.nic_mac.as_deref().map_or_else(
            || "virtio-net-pci,netdev=net0".into(),
            |mac| format!("virtio-net-pci,netdev=net0,mac={mac}"),
        )
    }

    /// A direct-kernel boot has no SMBIOS to read, so the root key and the device credentials
    /// ride on the kernel command line.
    fn kernel_cmdline(vm: &Vm<'_>, direct: &DirectKernel) -> String {
        let mut cmdline = direct.cmdline.clone();
        if let Some(public) = Self::public_key(vm) {
            cmdline.push(' ');
            cmdline.push_str(&sshkey::kernel_credential(&public));
        }
        for credential in device_credentials(vm.entry.ticket.as_deref()) {
            cmdline.push(' ');
            cmdline.push_str(&credential_kernel(&credential));
        }
        if let Some(partuuid) = &direct.loader_partuuid {
            cmdline.push_str(" systemd.setenv=TILO_ROOT_LOADER_PARTUUID=");
            cmdline.push_str(partuuid);
        }
        cmdline
    }

    fn qemu_args(vm: &Vm<'_>) -> Vec<String> {
        let disk = vm.entry.disk.as_deref().unwrap_or_default();
        let code = vm.entry.firmware_code.as_deref().unwrap_or_default();
        let vars = whpx_file(disk, "vars.fd");
        let tty = whpx_file(disk, "ttyS0.log");
        let uart = whpx_file(disk, "uart.log");
        let mut args: Vec<String> = vec![
            "-name".into(),
            vm.name.into(),
            "-machine".into(),
            "virt,gic-version=3,highmem=on".into(),
            "-accel".into(),
            "whpx".into(),
            "-cpu".into(),
            "host".into(),
            "-smp".into(),
            format!("cpus={}", vm.cpus()),
            "-m".into(),
            vm.memory().to_string(),
            "-no-reboot".into(),
        ];
        args.extend(Self::uuid_args(vm));
        if let Some(direct) = &vm.entry.direct_kernel {
            args.extend([
                "-kernel".into(),
                direct.kernel.clone(),
                "-initrd".into(),
                direct.initrd.clone(),
                "-append".into(),
                Self::kernel_cmdline(vm, direct),
            ]);
        } else {
            args.extend([
                "-drive".into(),
                format!("if=pflash,format=raw,readonly=on,file={code}"),
                "-drive".into(),
                format!("if=pflash,format=raw,file={vars}"),
            ]);
        }
        args.extend([
            "-drive".into(),
            format!("if=none,id=osdisk,format=qcow2,file={disk},cache=writeback,aio=threads"),
            "-device".into(),
            "virtio-blk-pci,drive=osdisk,bootindex=1".into(),
        ]);
        if let Some(esp) = &vm.entry.boot_esp
            && vm.entry.direct_kernel.is_none()
        {
            args.extend([
                "-drive".into(),
                format!("if=none,id=bootesp,format=raw,readonly=on,file={esp}"),
                "-device".into(),
                "virtio-blk-pci,drive=bootesp,bootindex=0".into(),
            ]);
        }
        if !vm.entry.headless {
            args.extend([
                "-device".into(),
                "virtio-gpu-pci,edid=on,xres=1280,yres=1024".into(),
                "-device".into(),
                "qemu-xhci".into(),
                "-device".into(),
                "usb-kbd".into(),
                "-device".into(),
                "usb-tablet".into(),
            ]);
        }
        args.extend([
            "-netdev".into(),
            format!(
                "user,id=net0,hostfwd=tcp:127.0.0.1:{port}-:22,hostfwd=udp:127.0.0.1:{port}-:{}",
                crate::config::DAEMON_PORT,
                port = vm.ssh_port()
            ),
            "-device".into(),
            Self::net_device(vm),
            "-serial".into(),
            format!("file:{uart}"),
            "-chardev".into(),
            format!("file,id=tilo-ttyS0,path={tty}"),
            "-device".into(),
            "pci-serial,chardev=tilo-ttyS0".into(),
            "-qmp".into(),
            format!("tcp:127.0.0.1:{},server=on,wait=off", Self::qmp_port(vm)),
            "-monitor".into(),
            "none".into(),
        ]);
        args.extend(if vm.entry.headless {
            ["-display".into(), "none".into()]
        } else {
            [
                "-display".into(),
                format!("vnc=127.0.0.1:{}", vm.ssh_port() % 1_000),
            ]
        });
        args
    }

    fn qmp(vm: &Vm<'_>, action: &str) -> CommandLine {
        let pid = ps_quote(&Self::pid(vm));
        let script = format!(
            "$ErrorActionPreference='Stop'; if (!(Test-Path -LiteralPath {pid})) {{ exit 0 }}; \
             $id=[int](Get-Content -LiteralPath {pid}); if (!(Get-Process -Id $id -ErrorAction SilentlyContinue)) {{ exit 0 }}; \
             $c=[Net.Sockets.TcpClient]::new('127.0.0.1',{}); \
             try {{ $c.ReceiveTimeout=3000; $s=$c.GetStream(); \
             $r=[IO.StreamReader]::new($s); $w=[IO.StreamWriter]::new($s); $w.AutoFlush=$true; \
             $null=$r.ReadLine(); $w.WriteLine('{{\"execute\":\"qmp_capabilities\"}}'); \
             $null=$r.ReadLine(); $w.WriteLine('{{\"execute\":\"{action}\"}}'); \
             $null=$r.ReadLine() }} finally {{ $c.Dispose() }}",
            Self::qmp_port(vm)
        );
        powershell(&script)
    }
}

impl Hypervisor for Whpx {
    fn name(&self) -> &'static str {
        "whpx"
    }

    fn create(&self, vm: &Vm<'_>, _root_key: &str) -> Result<Vec<CommandLine>, CliError> {
        let disk = vm.disk()?;
        let template = vm
            .entry
            .firmware_vars
            .as_deref()
            .ok_or_else(|| CliError::Config {
                path: crate::config::registry_path().display().to_string(),
                detail: "WHPX VM has no firmware_vars template".into(),
            })?;
        let mut commands: Vec<_> = overlay_disk(vm)?.into_iter().collect();
        commands.push(powershell(&format!(
            "Copy-Item -LiteralPath {} -Destination {} -ErrorAction Stop",
            ps_quote(template),
            ps_quote(&whpx_file(disk, "vars.fd"))
        )));
        Ok(commands)
    }

    fn start(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        let disk = vm.entry.disk.as_deref().unwrap_or_default();
        let qemu = vm
            .entry
            .qemu_system
            .as_deref()
            .unwrap_or("qemu-system-aarch64");
        let mut args = Self::qemu_args(vm);
        if vm.entry.direct_kernel.is_none()
            && let Some(public) = Self::public_key(vm)
        {
            args.extend([
                "-smbios".into(),
                format!("type=11,value={}", sshkey::smbios_credential(&public)),
            ]);
        }
        if vm.entry.direct_kernel.is_none() {
            args.extend(smbios_args(vm.entry.ticket.as_deref()));
        }
        let args = args
            .iter()
            .map(|s| win_arg(s))
            .collect::<Vec<_>>()
            .join(" ");
        let pid = ps_quote(&Self::pid(vm));
        let serial = ps_quote(&whpx_file(disk, "ttyS0.log"));
        // Keep Start-Process detached. Redirecting even one QEMU stream made
        // Command::output wait for the VM to exit on Windows ARM, although the
        // guest was already serving SSH. QEMU's serial devices log boot output.
        let script = format!(
            "$ErrorActionPreference='Stop'; \
             $free=([IO.DriveInfo]::new([IO.Path]::GetPathRoot({}))).AvailableFreeSpace; \
             if ($free -lt 15GB) {{ throw 'WHPX VM requires at least 15 GiB free on its disk volume' }}; \
             if (Test-Path -LiteralPath {pid}) {{ $old=[int](Get-Content -LiteralPath {pid}); \
             $oldProcess=Get-Process -Id $old -ErrorAction SilentlyContinue; \
             if ($oldProcess -and $oldProcess.ProcessName -eq 'qemu-system-aarch64') {{ throw 'WHPX VM is already running' }} }}; \
             $p=Start-Process -FilePath {} -ArgumentList {} -PassThru -WindowStyle Hidden; \
             Set-Content -LiteralPath {pid} -Value $p.Id; Start-Sleep -Seconds 1; \
             if ($p.HasExited) {{ throw ('QEMU exited immediately: '+$p.ExitCode+'; see '+{serial}) }}",
            ps_quote(disk),
            ps_quote(qemu),
            ps_quote(&args)
        );
        vec![powershell(&script)]
    }

    fn stop(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![Self::qmp(vm, "system_powerdown")]
    }

    fn destroy(&self, _vm: &Vm<'_>) -> Vec<CommandLine> {
        Vec::new()
    }

    fn branching(&self) -> Branching {
        Branching::Backing
    }

    fn power_off(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(Self::qmp(vm, "quit"))
    }

    fn ssh_endpoint(&self, vm: &Vm<'_>, _runner: &mut dyn Runner) -> Result<SshEndpoint, CliError> {
        Ok(configured_endpoint(vm, "127.0.0.1", vm.ssh_port()))
    }

    fn running(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Option<bool> {
        let pid = ps_quote(&Self::pid(vm));
        let script = format!(
            "if (Test-Path -LiteralPath {pid}) {{ $id=[int](Get-Content -LiteralPath {pid}); $p=Get-Process -Id $id -ErrorAction SilentlyContinue; if ($p -and $p.ProcessName -eq 'qemu-system-aarch64') {{ 'yes' }} else {{ 'no' }} }} else {{ 'no' }}"
        );
        runner
            .output(&powershell(&script))
            .ok()
            .map(|o| o.success && o.stdout.trim() == "yes")
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            serial: true,
            credentials: Some("kernel command line (direct boot) or QEMU SMBIOS type 11"),
            screenshot: true,
            direct_kernel: true,
            boot_esp: true,
            control: Some("QEMU monitor (HMP over QMP)"),
            ..Capabilities::default()
        }
    }

    fn serial(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(powershell(&format!(
            "Get-Content -LiteralPath {} -Wait -Tail 100",
            ps_quote(&whpx_file(vm.entry.disk.as_deref()?, "ttyS0.log"))
        )))
    }

    fn screenshot(&self, vm: &Vm<'_>, out: &Path) -> Option<CommandLine> {
        let script = format!(
            "$ErrorActionPreference='Stop'; \
             $out=[IO.Path]::GetFullPath({}); \
             $null=[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($out)); \
             $c=[Net.Sockets.TcpClient]::new('127.0.0.1',{}); \
             try {{ $c.ReceiveTimeout=5000; $c.SendTimeout=5000; $s=$c.GetStream(); \
             $r=[IO.StreamReader]::new($s); $w=[IO.StreamWriter]::new($s); $w.AutoFlush=$true; \
             $null=$r.ReadLine(); \
             foreach ($request in @(@{{execute='qmp_capabilities'}}, \
                 @{{execute='screendump';arguments=@{{filename=$out;format='png'}}}})) {{ \
               $w.WriteLine(($request | ConvertTo-Json -Compress -Depth 4)); \
               do {{ $line=$r.ReadLine(); if ($null -eq $line) {{ throw 'QMP connection closed' }}; \
                 $response=$line | ConvertFrom-Json; \
               }} while ($response.PSObject.Properties.Name -notcontains 'return' -and \
                        $response.PSObject.Properties.Name -notcontains 'error'); \
               if ($response.error) {{ throw ('QMP screenshot failed: '+$response.error.desc) }}; \
             }} }} finally {{ $c.Dispose() }}",
            ps_quote(&out.display().to_string()),
            Self::qmp_port(vm)
        );
        Some(powershell(&script))
    }

    fn control(&self, vm: &Vm<'_>, args: &[String]) -> Option<CommandLine> {
        let command = ps_quote(&args.join(" "));
        let script = format!(
            "$ErrorActionPreference='Stop'; \
             $c=[Net.Sockets.TcpClient]::new('127.0.0.1',{}); \
             try {{ $c.ReceiveTimeout=5000; $c.SendTimeout=5000; $s=$c.GetStream(); \
             $r=[IO.StreamReader]::new($s); $w=[IO.StreamWriter]::new($s); $w.AutoFlush=$true; \
             $null=$r.ReadLine(); \
             foreach ($request in @(@{{execute='qmp_capabilities'}}, \
                 @{{execute='human-monitor-command';arguments=@{{'command-line'={command}}}}})) {{ \
               $w.WriteLine(($request | ConvertTo-Json -Compress -Depth 4)); \
               do {{ $line=$r.ReadLine(); if ($null -eq $line) {{ throw 'QMP connection closed' }}; \
                 $response=$line | ConvertFrom-Json; \
               }} while ($response.PSObject.Properties.Name -notcontains 'return' -and \
                        $response.PSObject.Properties.Name -notcontains 'error'); \
               if ($response.error) {{ throw ('QMP monitor failed: '+$response.error.desc) }}; \
               if ($request.execute -eq 'human-monitor-command') {{ $response.return }}; \
             }} }} finally {{ $c.Dispose() }}",
            Self::qmp_port(vm)
        );
        Some(powershell(&script))
    }
}

pub struct HyperV;

fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn powershell(script: &str) -> CommandLine {
    CommandLine::new(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", script],
    )
}

fn serial_pipe(name: &str) -> String {
    format!(r"\\.\pipe\tilo-vm-{name}")
}

impl Hypervisor for HyperV {
    fn name(&self) -> &'static str {
        "hyperv"
    }

    fn create(&self, vm: &Vm<'_>, _root_key: &str) -> Result<Vec<CommandLine>, CliError> {
        let name = ps_quote(vm.name);
        let disk = ps_quote(vm.disk()?);
        let mut commands = Vec::new();
        if let Some(image) = &vm.entry.image {
            commands.push(powershell(&format!(
                "Copy-Item -LiteralPath {} -Destination {disk}",
                ps_quote(image)
            )));
        }
        commands.push(powershell(&format!(
            "New-VM -Name {name} -Generation 2 -MemoryStartupBytes {}MB -VHDPath {disk} -SwitchName 'Default Switch'; \
             Set-VMProcessor -VMName {name} -Count {}; \
             Set-VMFirmware -VMName {name} -EnableSecureBoot Off; \
             Set-VMComPort -VMName {name} -Number 1 -Path {}",
            vm.memory(),
            vm.cpus(),
            ps_quote(&serial_pipe(vm.name)),
        )));
        Ok(commands)
    }

    fn start(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![powershell(&format!("Start-VM -Name {}", ps_quote(vm.name)))]
    }

    fn stop(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![powershell(&format!("Stop-VM -Name {}", ps_quote(vm.name)))]
    }

    fn destroy(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        let name = ps_quote(vm.name);
        vec![powershell(&format!("Remove-VM -Name {name} -Force"))]
    }

    fn branching(&self) -> Branching {
        Branching::Unsupported(
            "a Hyper-V VM runs its own copy of the VHDX, which has no copy-on-write child here; on Windows ARM64 use the whpx adapter: tilo-vm vm create <name> --hypervisor whpx".into(),
        )
    }

    fn power_off(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(powershell(&format!(
            "Stop-VM -Name {} -TurnOff -Force",
            ps_quote(vm.name)
        )))
    }

    fn ssh_endpoint(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Result<SshEndpoint, CliError> {
        if let Some(endpoint) = vm.entry.ssh.as_deref().and_then(SshEndpoint::parse) {
            return Ok(endpoint);
        }
        let command = powershell(&format!(
            "(Get-VMNetworkAdapter -VMName {}).IPAddresses | Where-Object {{ $_ -match '^[0-9.]+$' }} | Select-Object -First 1",
            ps_quote(vm.name)
        ));
        let output = runner.output(&command)?;
        match output.stdout.trim() {
            host if output.success && !host.is_empty() => {
                Ok(SshEndpoint { user: "root".into(), host: host.into(), port: 22 })
            }
            _ => Err(CliError::Unreachable {
                vm: vm.name.into(),
                detail: "Hyper-V reported no IPv4 address (is the VM running with integration services?)".into(),
                debug: vec![format!("tilo-vm vm serial {}", vm.name)],
            }),
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            serial: true,
            screenshot: false,
            reset: true,
            credentials: None,
            control: Some("PowerShell (Hyper-V module)"),
            direct_kernel: false,
            boot_esp: false,
        }
    }

    fn serial(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(CommandLine::new(
            "plink.exe",
            &["-serial", &serial_pipe(vm.name)],
        ))
    }

    fn reset(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(powershell(&format!(
            "Restart-VM -Name {} -Force",
            ps_quote(vm.name)
        )))
    }

    fn control(&self, _vm: &Vm<'_>, args: &[String]) -> Option<CommandLine> {
        Some(powershell(&args.join(" ")))
    }
}
