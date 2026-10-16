//! Thin adapters over each host's native hypervisor tooling. Humans keep
//! using the hypervisor's own UI; tilo-vm only needs create, start, stop,
//! destroy and the SSH endpoint, plus optional debugging capabilities for
//! when SSH is not reachable.
//!
//! Adapters only *build* command lines; [`Runner`] executes them, so the
//! generated commands are unit-tested on every platform.

use std::path::Path;

use serde::Serialize;

use crate::config::VmEntry;
use crate::error::CliError;
use crate::image;
use crate::sshkey;

/// One external command.
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

/// Executes command lines; faked in tests.
pub trait Runner {
    /// Run to completion, capturing output.
    ///
    /// # Errors
    /// The program could not be started.
    fn output(&mut self, command: &CommandLine) -> Result<Output, CliError>;
    /// Run attached to the terminal.
    ///
    /// # Errors
    /// The program could not be started.
    fn interactive(&mut self, command: &CommandLine) -> Result<bool, CliError>;
}

pub struct SystemRunner {
    pub cwd: Option<std::path::PathBuf>,
}

impl SystemRunner {
    fn command(&self, line: &CommandLine) -> std::process::Command {
        let mut command = std::process::Command::new(&line.program);
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

/// Run each command in order, stopping at the first failure.
///
/// # Errors
/// The first command that fails.
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

/// What an adapter can do beyond the required five operations.
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
    /// Native control channel passed through by `vm control`.
    pub control: Option<&'static str>,
    /// Boots `VmEntry::direct_kernel` from the host (`swap kernel --direct`).
    pub direct_kernel: bool,
    /// Attaches `VmEntry::boot_esp` read-only ahead of the VM's disk.
    pub boot_esp: bool,
}

/// A VM as the adapter sees it.
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

    fn cpus(&self) -> u32 {
        self.entry.cpus.unwrap_or(4)
    }

    fn ssh_port(&self) -> u16 {
        self.entry.ssh_port.unwrap_or(2222)
    }
}

pub trait Hypervisor {
    fn name(&self) -> &'static str;
    /// Commands that create the VM from its image. `root_key` is the public
    /// key line for root's `authorized_keys`, passed on where the adapter has
    /// a credential channel ([`Capabilities::credentials`]).
    ///
    /// # Errors
    /// Missing registry fields.
    fn create(&self, vm: &Vm<'_>, root_key: &str) -> Result<Vec<CommandLine>, CliError>;
    fn start(&self, vm: &Vm<'_>) -> Vec<CommandLine>;
    fn stop(&self, vm: &Vm<'_>) -> Vec<CommandLine>;
    /// A power-button event can suspend an ARM guest rather than power it off.
    /// Prefer an explicit guest shutdown when this adapter has that behaviour.
    fn guest_shutdown_first(&self) -> bool {
        false
    }
    /// Commands that unregister a VM that is off from the hypervisor. The
    /// files it owns ([`vm_files`]) are removed by the caller.
    fn destroy(&self, vm: &Vm<'_>) -> Vec<CommandLine>;
    /// Where SSH listens; may query the hypervisor.
    ///
    /// # Errors
    /// The hypervisor could not report an address.
    fn ssh_endpoint(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Result<SshEndpoint, CliError>;

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    /// Is the VM running, as the hypervisor sees it? `None` when the adapter
    /// cannot tell.
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
    /// Commands that rewrite an existing VM's definition from its registry
    /// entry (for `direct_kernel`), where [`Capabilities::direct_kernel`].
    /// `runner` may query the hypervisor for the VM's identity.
    ///
    /// # Errors
    /// Missing registry fields.
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
    /// Power the VM off at once, so the next start takes a new definition.
    fn power_off(&self, _vm: &Vm<'_>) -> Option<CommandLine> {
        None
    }
    /// Commands that wake the VM from a suspend (ACPI S3) it went into
    /// itself; `None` when this adapter cannot (a device is woken with its
    /// magic packet instead).
    fn wake(&self, _vm: &Vm<'_>) -> Option<Vec<CommandLine>> {
        None
    }
    /// Is the VM suspended, as the hypervisor sees it? `None` when the
    /// adapter cannot tell.
    fn suspended(&self, _vm: &Vm<'_>, _runner: &mut dyn Runner) -> Option<bool> {
        None
    }
    /// Can tilo-vm power the machine on? A physical device it only reaches
    /// over SSH stays off once off, so it is restarted with a reboot.
    fn powers_on(&self) -> bool {
        true
    }
    /// Does a CI layer come out of an image this host pulls, rather than
    /// one the guest pulls? A device's storage and network are a board's:
    /// it gets only the layer, never the whole multi-gigabyte image.
    fn ci_layers_from_host(&self) -> bool {
        false
    }
}

/// # Errors
/// Unknown hypervisor names.
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

/// A qcow2 child of the image (a qcow2 dev disk or a raw disk), so the image
/// stays pristine. Runs the `qemu-img` resolved when the VM was created.
fn overlay_disk(vm: &Vm<'_>) -> Result<Option<CommandLine>, CliError> {
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

/// Every file a VM created by `vm create` owns: its disk and what the
/// adapters keep beside it (libvirt's domain XML; WHPX's UEFI variables,
/// pid file and serial logs). Some exist only for some adapters.
#[must_use]
pub fn vm_files(disk: &str) -> Vec<String> {
    let mut files = vec![disk.to_owned(), domain_xml_path(disk)];
    files.extend(
        ["vars.fd", "pid", "ttyS0.log", "uart.log"]
            .iter()
            .map(|suffix| whpx_file(disk, suffix)),
    );
    files
}

// ---- a dev device reached over SSH (a board) ---------------------------------

/// The adapter of a machine that already runs a Tilo dev image, such as a
/// board installed from its board dev disk, registered with `vm add`.
pub const DEVICE: &str = "device";

/// No hypervisor: tilo-vm reaches the device only at its SSH address. It
/// cannot create, power on or reset it; `vm stop` shuts it down over SSH
/// and `vm restart` reboots it.
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

    fn guest_shutdown_first(&self) -> bool {
        true
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
}

// ---- libvirt (Linux) ----------------------------------------------------------

pub struct Libvirt;

impl Libvirt {
    /// Write the domain XML beside the disk; the command that defines it.
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

/// Where `create` writes the libvirt domain definition: beside the VM disk.
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
///
/// # Errors
/// The VM has no `disk`.
pub fn domain_xml(vm: &Vm<'_>, root_key: &str) -> Result<String, CliError> {
    domain_xml_with_uuid(vm, root_key, None)
}

/// [`domain_xml`] for a domain that already exists: libvirt redefines a
/// domain only under its own UUID.
///
/// # Errors
/// The VM has no `disk`.
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
    Ok(format!(
        "<domain type='{domain_type}'>
  <name>{name}</name>{uuid}
  <memory unit='MiB'>{memory}</memory>
  <vcpu>{cpus}</vcpu>
  <os firmware='efi'>
    <type arch='{arch}' machine='{machine}'>hvm</type>
    <firmware>
      <feature enabled='no' name='secure-boot'/>
      <feature enabled='no' name='enrolled-keys'/>
    </firmware>
    <smbios mode='sysinfo'/>{direct}
  </os>
  <sysinfo type='smbios'>
    <oemStrings>
      <entry>{credential}</entry>
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
    <interface type='user'>
      <backend type='passt'/>
      <model type='virtio'/>
      <portForward proto='tcp'><range start='{port}' to='22'/></portForward>
    </interface>
    <serial type='pty'/>
    <console type='pty'/>
    <controller type='usb' model='qemu-xhci'/>
    <input type='tablet' bus='usb'/>
    <input type='keyboard' bus='usb'/>
    <graphics type='spice' autoport='yes'/>{gl}
    <video>{video}</video>
    <rng model='virtio'><backend model='random'>/dev/urandom</backend></rng>
  </devices>
</domain>
",
        name = e(vm.name),
        uuid = uuid.map_or_else(String::new, |u| format!("\n  <uuid>{}</uuid>", e(u))),
        memory = vm.memory(),
        cpus = vm.cpus(),
        arch = e(arch),
        credential = e(&sshkey::smbios_credential(root_key)),
        disk = e(vm.disk()?),
        port = vm.ssh_port(),
        video = video_model(vm.entry),
        gl = if vm.entry.gl {
            "\n    <graphics type='egl-headless'/>"
        } else {
            ""
        },
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

/// libvirt's virtio-gpu: `heads` outputs, the first preferring `resolution`,
/// with virgl when `gl`.
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
        format!("<model type='virtio'{heads}/>")
    } else {
        format!("<model type='virtio'{heads}>{accel}{resolution}</model>")
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
        let uuid = runner
            .output(&Self::virsh(vm, &["domuuid", vm.name]))
            .ok()
            .filter(|o| o.success)
            .map(|o| o.stdout.trim().to_owned())
            .filter(|u| !u.is_empty());
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

// ---- raw QEMU via os/dev/vm.sh (Linux, CI and scenario tests) ---------------

/// The headless adapter for CI and scenario tests: it drives `os/dev/vm.sh`
/// from the worktree root, with that script's own conventions. A VM made
/// this way is invisible to virt-manager and Boxes, so it is never the
/// default; people use libvirt.
pub struct Qemu;

impl Qemu {
    fn script(vm: &Vm<'_>, args: &[&str]) -> CommandLine {
        CommandLine::new("bash", &["os/dev/vm.sh"])
            .args_from(args)
            .env("TILO_SSH_PORT", vm.ssh_port().to_string())
    }
}

impl Hypervisor for Qemu {
    fn name(&self) -> &'static str {
        "qemu"
    }

    fn create(&self, vm: &Vm<'_>, _root_key: &str) -> Result<Vec<CommandLine>, CliError> {
        // os/dev/vm.sh takes no extra QEMU arguments, so no credential.
        Ok(overlay_disk(vm)?.into_iter().collect())
    }

    fn start(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        let disk = vm.entry.disk.clone().unwrap_or_default();
        let display = if vm.entry.gl { "--gl" } else { "--2d" };
        let mut command = Self::script(vm, &["boot", display, "--disk", &disk])
            .env("TILO_MEM", vm.memory().to_string())
            .env("TILO_SMP", vm.cpus().to_string());
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
        vec![Self::script(vm, &["shutdown"])]
    }

    fn destroy(&self, _vm: &Vm<'_>) -> Vec<CommandLine> {
        // os/dev/vm.sh keeps no registration beside the disk.
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
            credentials: None,
            control: Some("QEMU monitor (HMP)"),
            direct_kernel: false,
            boot_esp: false,
        }
    }

    fn serial(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(Self::script(vm, &["serial"]))
    }

    fn screenshot(&self, vm: &Vm<'_>, out: &Path) -> Option<CommandLine> {
        // vm.sh writes $TILO_RUN/<name>.png.
        let dir = out
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let stem = out
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Some(Self::script(vm, &["shot", &stem]).env("TILO_RUN", dir))
    }

    fn reset(&self, vm: &Vm<'_>) -> Option<CommandLine> {
        Some(Self::script(vm, &["cmd", "system_reset"]))
    }

    fn control(&self, vm: &Vm<'_>, args: &[String]) -> Option<CommandLine> {
        Some(Self::script(vm, &["cmd"]).arg(args.join(" ")))
    }
}

// ---- UTM (macOS) --------------------------------------------------------------

pub struct Utm;

/// What a UTM VM gets unless `--memory-mb`/`--cpus` say otherwise: building
/// the shell in the VM needs more than the other adapters' default (a
/// release link of `tilo-relay` alone was killed at 6 GiB).
pub const UTM_MEMORY_MB: u32 = 16384;
pub const UTM_CPUS: u32 = 8;

/// Import `disk` (qcow2) into UTM as the template VM `name`: an aarch64 guest
/// on Apple's hypervisor with UEFI, `memory_mb` and `cpus`, and one virtio
/// disk, on UTM's shared network. UTM copies the disk into the VM's bundle.
///
/// # Errors
///
/// UTM refused the import, or `osascript` could not run.
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

/// The IPv4 address macOS leased the UTM VM `vm`, found by the MAC address of
/// its first network interface (asked of UTM over `AppleScript`).
fn utm_leased_address(vm: &Vm<'_>, runner: &mut dyn Runner) -> Option<String> {
    // A VM's configuration is a record: read it whole, then its first
    // interface's address.
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

/// The `ip_address` of the newest lease in `leases` (`dhcpd_leases`' format)
/// for hardware address `mac`. macOS drops leading zeros of each octet
/// (`6a:1c:d:58:e4:14`), so octets compare as numbers.
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
            // Entries run newest first.
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
            // `ip-address` needs the QEMU guest agent, which a Tilo image
            // does not run: on UTM's shared network, macOS's DHCP server
            // leased the VM its address, so find it there by the VM's MAC.
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

    fn control(&self, _vm: &Vm<'_>, args: &[String]) -> Option<CommandLine> {
        let mut command = CommandLine::new("utmctl", &[]);
        command.args.extend(args.iter().cloned());
        Some(command)
    }
}

// ---- QEMU with WHPX (Windows ARM64) -----------------------------------------

/// Windows ARM64 Home has WHPX even though it has no Hyper-V management
/// module. QEMU is launched detached; the published qcow2 stays read-only.
pub struct Whpx;

fn whpx_file(disk: &str, suffix: &str) -> String {
    format!("{disk}.{suffix}")
}

/// Quote one argument for the Windows C runtime command-line parser.
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
        if let Some(direct) = &vm.entry.direct_kernel {
            let mut cmdline = direct.cmdline.clone();
            if let Some(public) = Self::public_key(vm) {
                cmdline.push(' ');
                cmdline.push_str(&sshkey::kernel_credential(&public));
            }
            if let Some(partuuid) = &direct.loader_partuuid {
                cmdline.push_str(" systemd.setenv=TILO_ROOT_LOADER_PARTUUID=");
                cmdline.push_str(partuuid);
            }
            args.extend([
                "-kernel".into(),
                direct.kernel.clone(),
                "-initrd".into(),
                direct.initrd.clone(),
                "-append".into(),
                cmdline,
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
            format!("user,id=net0,hostfwd=tcp:127.0.0.1:{}-:22", vm.ssh_port()),
            "-device".into(),
            "virtio-net-pci,netdev=net0".into(),
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

    fn guest_shutdown_first(&self) -> bool {
        true
    }

    fn destroy(&self, _vm: &Vm<'_>) -> Vec<CommandLine> {
        // QEMU keeps no registration: the VM is its files.
        Vec::new()
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

// ---- Hyper-V (Windows) ----------------------------------------------------------

pub struct HyperV;

/// A `PowerShell` single-quoted string literal.
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
        // No credential channel: Hyper-V sets no SMBIOS OEM strings, and its
        // KVP and file-copy services need guest daemons the image lacks.
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
        // The COM1 named pipe set at creation; PuTTY's plink speaks it.
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

#[cfg(test)]
mod tests {
    #[test]
    fn a_utm_vm_is_found_in_macos_dhcp_leases_by_its_mac() {
        let leases = "{\n\tname=fedora\n\tip_address=192.168.64.7\n\thw_address=1,6a:1c:ad:58:e4:14\n\tidentifier=1,6a:1c:ad:58:e4:14\n\tlease=0x6acedea9\n}\n{\n\tip_address=192.168.64.2\n\thw_address=1,6a:1c:ad:58:e4:14\n\tlease=0x6acedea8\n}\n{\n\tip_address=192.168.64.3\n\thw_address=1,2:0:0:0:0:1\n}\n";
        assert_eq!(
            super::leased_address(leases, "6A:1C:AD:58:E4:14").as_deref(),
            Some("192.168.64.7"),
            "the newest lease of that MAC"
        );
        assert_eq!(
            super::leased_address(leases, "02:00:00:00:00:01").as_deref(),
            Some("192.168.64.3"),
            "octets without leading zeros match"
        );
        assert_eq!(super::leased_address(leases, "02:00:00:00:00:02"), None);
    }

    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample tilo-vm-dev";

    struct Fake(Vec<CommandLine>, Output);

    impl Runner for Fake {
        fn output(&mut self, command: &CommandLine) -> Result<Output, CliError> {
            self.0.push(command.clone());
            Ok(self.1.clone())
        }
        fn interactive(&mut self, command: &CommandLine) -> Result<bool, CliError> {
            self.0.push(command.clone());
            Ok(true)
        }
    }

    #[test]
    fn whpx_monitor_keeps_command_arguments_in_a_literal() {
        let entry = entry();
        let vm = Vm {
            name: "dev",
            entry: &entry,
        };
        let command = Whpx
            .control(&vm, &["help '; throw 'injected'; #".into()])
            .expect("WHPX monitor is supported");
        let script = command.args.last().expect("PowerShell script");
        assert!(script.contains("'command-line'='help ''; throw ''injected''; #'"));
        assert!(Whpx.capabilities().control.is_some());
    }

    fn fake(stdout: &str) -> Fake {
        Fake(
            Vec::new(),
            Output {
                success: true,
                status: "0".into(),
                stdout: stdout.into(),
                stderr: String::new(),
            },
        )
    }

    #[test]
    fn utm_import_uses_native_scripting_with_escaped_inputs_and_requested_resources() {
        let mut runner = fake("");
        utm_import_template(
            &mut runner,
            ("Tilo Dev (\"test\")", "/images/a\\b\"c.qcow2"),
            (8192, 4),
        )
        .unwrap();
        assert_eq!(runner.0.len(), 1);
        let command = &runner.0[0];
        assert_eq!(command.program, "osascript");
        assert_eq!(
            command.args,
            [
                "-e",
                r#"set diskFile to POSIX file "/images/a\\b\"c.qcow2""#,
                "-e",
                "tell application \"UTM\"",
                "-e",
                r#"make new virtual machine with properties {backend:qemu, configuration:{name:"Tilo Dev (\"test\")", architecture:"aarch64", memory:8192, cpu cores:4, hypervisor:true, uefi:true, drives:{{interface:virtio, source:diskFile}}}}"#,
                "-e",
                "end tell",
            ]
        );
    }

    #[test]
    fn utm_import_reports_native_automation_refusal() {
        let mut runner = fake("");
        runner.1.success = false;
        runner.1.stderr = "Not authorized to send Apple events to UTM".into();
        let error = utm_import_template(
            &mut runner,
            ("Tilo Dev (test)", "/images/dev.qcow2"),
            (UTM_MEMORY_MB, UTM_CPUS),
        )
        .unwrap_err();
        assert!(matches!(error, CliError::Unreachable { .. }));
        assert!(error.to_string().contains(&runner.1.stderr));
        assert!(runner.0[0].args[5].contains("memory:16384, cpu cores:8"));
    }

    #[test]
    fn utm_explicit_ssh_does_not_query_the_hypervisor_or_host_leases() {
        let entry = VmEntry {
            ssh: Some("developer@192.0.2.4:2222".into()),
            ..entry()
        };
        let mut runner = fake("");
        let endpoint = Utm
            .ssh_endpoint(
                &Vm {
                    name: "dev",
                    entry: &entry,
                },
                &mut runner,
            )
            .unwrap();
        assert_eq!(endpoint.user, "developer");
        assert_eq!(endpoint.host, "192.0.2.4");
        assert_eq!(endpoint.port, 2222);
        assert!(runner.0.is_empty());
    }

    fn entry() -> VmEntry {
        VmEntry {
            hypervisor: String::new(),
            image: Some("/img/tilo-vm.qcow2".into()),
            disk: Some("/vms/dev.qcow2".into()),
            memory_mb: Some(4096),
            cpus: Some(2),
            ssh_port: Some(2223),
            ..VmEntry::default()
        }
    }

    #[test]
    fn whpx_defaults_to_fast_memory_without_changing_explicit_or_other_vms() {
        let memory = |entry: &VmEntry| Vm { name: "dev", entry }.memory();
        let mut entry = entry();
        entry.hypervisor = "whpx".into();
        entry.memory_mb = None;
        assert_eq!(memory(&entry), 3072);

        entry.memory_mb = Some(8192);
        assert_eq!(memory(&entry), 8192);

        entry.hypervisor = "libvirt".into();
        entry.memory_mb = None;
        assert_eq!(memory(&entry), 8192);
    }

    #[test]
    fn qemu_start_uses_registered_fixture_without_changing_runtime_control() {
        let mut entry = VmEntry {
            hypervisor: "qemu".into(),
            memory_mb: Some(3072),
            cpus: Some(3),
            emulated: true,
            arch: Some("aarch64".into()),
            resolution: Some("1920x1080".into()),
            ..entry()
        };
        let start = |entry: &VmEntry| Qemu.start(&Vm { name: "dev", entry })[0].display();
        assert_eq!(
            start(&entry),
            "TILO_SSH_PORT=2223 TILO_MEM=3072 TILO_SMP=3 TILO_ACCEL=tcg TILO_VM_ARCH=aarch64 TILO_XRES=1920 TILO_YRES=1080 bash os/dev/vm.sh boot --2d --disk /vms/dev.qcow2"
        );
        entry.gl = true;
        assert!(start(&entry).contains("boot --gl --disk"));
        entry.emulated = false;
        assert!(!start(&entry).contains("TILO_ACCEL="));
        entry.memory_mb = None;
        entry.cpus = None;
        assert!(start(&entry).contains("TILO_MEM=8192 TILO_SMP=4"));
        for invalid in ["invalid", "0x1080", "1920x0", "1920x1080x2"] {
            entry.resolution = Some(invalid.into());
            let command = start(&entry);
            assert!(!command.contains("TILO_XRES="), "{invalid}");
            assert!(!command.contains("TILO_YRES="), "{invalid}");
        }
        assert_eq!(
            Qemu.stop(&Vm {
                name: "dev",
                entry: &entry
            })[0]
                .display(),
            "TILO_SSH_PORT=2223 bash os/dev/vm.sh shutdown"
        );
    }

    /// WHPX uses a child, fresh vars and hardware acceleration, and headless direct boot preserves an
    /// explicit ESP binding.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn whpx_runs_a_fresh_accelerated_child_with_its_esp_binding() {
        // Phase: whpx_uses_a_child_fresh_vars_and_hardware_acceleration.
        {
            let temp = tempfile::tempdir().unwrap();
            let key = temp.path().join("vm_key");
            std::fs::write(format!("{}.pub", key.display()), KEY).unwrap();
            let entry = VmEntry {
                disk: Some(r"C:\vms\dev.qcow2".into()),
                image: Some(r"C:\images\base.qcow2".into()),
                qemu_img: Some(r"C:\msys64\clangarm64\bin\qemu-img.exe".into()),
                qemu_system: Some(r"C:\msys64\clangarm64\bin\qemu-system-aarch64.exe".into()),
                firmware_code: Some(r"C:\msys64\clangarm64\share\qemu\edk2-aarch64-code.fd".into()),
                firmware_vars: Some(r"C:\msys64\clangarm64\share\qemu\edk2-arm-vars.fd".into()),
                key: Some(key.display().to_string()),
                ..entry()
            };
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            let create = Whpx.create(&vm, KEY).unwrap();
            assert_eq!(create.len(), 2);
            assert_eq!(create[0].program, entry.qemu_img.clone().unwrap());
            assert!(create[0].args.contains(&"-b".into()));
            assert!(create[1].args.last().unwrap().contains("vars.fd"));
            let args = Whpx::qemu_args(&vm);
            assert!(args.windows(2).any(|p| p == ["-accel", "whpx"]));
            assert!(args.windows(2).any(|p| p == ["-cpu", "host"]));
            assert!(
                args.iter()
                    .any(|a| a.contains("pci-serial,chardev=tilo-ttyS0"))
            );
            assert!(args.iter().any(|a| a == "vnc=127.0.0.1:223"));
            assert!(!args.iter().any(|a| a.contains("tcg")));
            let start = Whpx.start(&vm);
            assert!(
                start[0]
                    .args
                    .last()
                    .unwrap()
                    .contains("io.systemd.credential.binary")
            );
            assert!(start[0].args.last().unwrap().contains("15GB"));
            assert!(!start[0].args.last().unwrap().contains("-RedirectStandard"));
            assert!(start[0].args.last().unwrap().contains("ttyS0.log"));
            assert!(
                Whpx.stop(&vm)[0]
                    .args
                    .last()
                    .unwrap()
                    .contains("system_powerdown")
            );
            assert!(
                Whpx.stop(&vm)[0]
                    .args
                    .last()
                    .unwrap()
                    .contains("$r.ReadLine()")
            );
            assert_eq!(Whpx.ssh_endpoint(&vm, &mut fake("")).unwrap().port, 2223);
        }

        // Phase: whpx_headless_direct_boot_preserves_explicit_esp_binding.
        {
            let entry = VmEntry {
                disk: Some(r"C:\vms\dev.qcow2".into()),
                boot_esp: Some(r"C:\vms\override.raw".into()),
                headless: true,
                direct_kernel: Some(crate::config::DirectKernel {
                    kernel: r"C:\bundle\Image".into(),
                    initrd: r"C:\bundle\initrd".into(),
                    cmdline: "root=tilo-auto composefs=abc".into(),
                    loader_partuuid: Some("fcdfc623-0563-4f91-9db8-f2b87b98b3ce".into()),
                    kver: "7.2.5".into(),
                }),
                ..entry()
            };
            let args = Whpx::qemu_args(&Vm {
                name: "dev",
                entry: &entry,
            });
            assert!(
                args.windows(2)
                    .any(|p| p == ["-kernel", r"C:\bundle\Image"])
            );
            assert!(
                args.windows(2)
                    .any(|p| p == ["-initrd", r"C:\bundle\initrd"])
            );
            assert!(args.windows(2).any(|p| p == [
                "-append",
                "root=tilo-auto composefs=abc systemd.setenv=TILO_ROOT_LOADER_PARTUUID=fcdfc623-0563-4f91-9db8-f2b87b98b3ce"
            ]));
            assert!(args.windows(2).any(|p| p == ["-display", "none"]));
            assert!(
                !args
                    .iter()
                    .any(|a| a.contains("pflash") || a.contains("bootesp"))
            );
            assert!(
                !args
                    .iter()
                    .any(|a| a.contains("virtio-gpu") || a.contains("usb-"))
            );
            assert!(!args.iter().any(|a| a.contains("vnc=")));
            assert!(Whpx.capabilities().direct_kernel);

            let temp = tempfile::tempdir().unwrap();
            let key = temp.path().join("direct_key");
            std::fs::write(format!("{}.pub", key.display()), format!("{KEY}\n")).unwrap();
            let keyed = VmEntry {
                key: Some(key.display().to_string()),
                ..entry
            };
            let keyed_vm = Vm {
                name: "dev",
                entry: &keyed,
            };
            let keyed_args = Whpx::qemu_args(&keyed_vm);
            let cmdline = keyed_args
                .windows(2)
                .find(|pair| pair[0] == "-append")
                .map(|pair| pair[1].as_str())
                .unwrap();
            assert!(
                cmdline.contains(&sshkey::kernel_credential(KEY)),
                "{cmdline}"
            );
            assert!(!Whpx.start(&keyed_vm)[0].display().contains("-smbios"));
        }
    }

    fn vm_xml(entry: &VmEntry) -> String {
        domain_xml(&Vm { name: "dev", entry }, KEY).unwrap()
    }

    /// Domain XML for `x86_64`, for `aarch64` and when emulated with TCG, with every value escaped.
    #[test]
    fn libvirt_domain_xml_matches_the_architecture_and_escapes_values() {
        // Phase: domain_xml_for_x86_64.
        {
            let entry = VmEntry {
                arch: Some("x86_64".into()),
                ..entry()
            };
            let xml = vm_xml(&entry);
            for part in [
                "<domain type='kvm'>",
                "<name>dev</name>",
                "<memory unit='MiB'>4096</memory>",
                "<vcpu>2</vcpu>",
                "<os firmware='efi'>",
                "<type arch='x86_64' machine='q35'>hvm</type>",
                "<feature enabled='no' name='secure-boot'/>",
                "<smbios mode='sysinfo'/>",
                "<features><acpi/><apic/></features>",
                "<cpu mode='host-passthrough'/>",
                "<driver name='qemu' type='qcow2' discard='unmap'/>",
                "<source file='/vms/dev.qcow2'/>",
                "<target dev='vda' bus='virtio'/>",
                "<backend type='passt'/>",
                "<portForward proto='tcp'><range start='2223' to='22'/></portForward>",
                "<serial type='pty'/>",
                "<graphics type='spice' autoport='yes'/>",
                "<video><model type='virtio'/></video>",
                "<rng model='virtio'>",
            ] {
                assert!(xml.contains(part), "no {part} in\n{xml}");
            }
            assert!(
                xml.contains(&format!(
                    "<entry>io.systemd.credential.binary:ssh.authorized_keys.root={}</entry>",
                    sshkey::encoded(KEY)
                )),
                "{xml}"
            );
            assert!(!xml.contains("machine='virt'"), "{xml}");
        }

        // Phase: domain_xml_for_aarch64.
        {
            let entry = VmEntry {
                arch: Some("aarch64".into()),
                ..entry()
            };
            let xml = vm_xml(&entry);
            assert!(xml.contains("<domain type='kvm'>"), "{xml}");
            assert!(
                xml.contains("<type arch='aarch64' machine='virt'>hvm</type>"),
                "{xml}"
            );
            assert!(xml.contains("<os firmware='efi'>"), "{xml}");
            assert!(xml.contains("<features><acpi/></features>"), "{xml}");
            assert!(!xml.contains("<apic/>"), "{xml}");
            assert!(xml.contains("<cpu mode='host-passthrough'/>"), "{xml}");
            // libvirt adds no USB controller to an aarch64 `virt` machine and then
            // refuses the USB tablet and keyboard.
            assert!(
                xml.contains("<controller type='usb' model='qemu-xhci'/>"),
                "{xml}"
            );
        }

        // Phase: domain_xml_when_emulated_uses_tcg.
        {
            let entry = VmEntry {
                arch: Some("aarch64".into()),
                emulated: true,
                ..entry()
            };
            let xml = vm_xml(&entry);
            assert!(xml.starts_with("<domain type='qemu'>"), "{xml}");
            assert!(xml.contains("<cpu mode='maximum'/>"), "{xml}");
            assert!(!xml.contains("host-passthrough"), "{xml}");
            assert!(xml.contains("machine='virt'"), "{xml}");
        }

        // Phase: domain_xml_escapes_every_value.
        {
            let entry = VmEntry {
                arch: Some("x86_64".into()),
                disk: Some("/vms/it's <a> & \"b\".qcow2".into()),
                ..entry()
            };
            let xml = domain_xml(
                &Vm {
                    name: "a&b<c>",
                    entry: &entry,
                },
                KEY,
            )
            .unwrap();
            assert!(xml.contains("<name>a&amp;b&lt;c&gt;</name>"), "{xml}");
            assert!(
                xml.contains("<source file='/vms/it&apos;s &lt;a&gt; &amp; &quot;b&quot;.qcow2'/>"),
                "{xml}"
            );
        }
    }

    /// libvirt create writes the domain beside the disk and defines it; libvirt commands; a raw image
    /// backs a qcow2 child; a boot ESP is attached read-only and boots first; removing a VM unregisters it
    /// and names its files; libvirt boots a direct kernel from the registry entry.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn libvirt_defines_boots_and_removes_vms_beside_their_disks() {
        // Phase: libvirt_create_writes_the_domain_beside_the_disk_and_defines_it.
        {
            let dir = tempfile::tempdir().unwrap();
            let disk = dir.path().join("dev.qcow2").display().to_string();
            let entry = VmEntry {
                disk: Some(disk.clone()),
                ..entry()
            };
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            let create = Libvirt.create(&vm, KEY).unwrap();
            let displayed: Vec<String> = create.iter().map(CommandLine::display).collect();
            assert_eq!(
                displayed,
                [
                    format!("qemu-img create -f qcow2 -F qcow2 -b /img/tilo-vm.qcow2 {disk}"),
                    format!("virsh --connect qemu:///session define {disk}.xml"),
                ]
            );
            let written = std::fs::read_to_string(format!("{disk}.xml")).unwrap();
            assert_eq!(written, domain_xml(&vm, KEY).unwrap());
            assert!(displayed.iter().all(|c| !c.contains("virt-install")));
        }

        // Phase: libvirt_commands.
        {
            let entry = entry();
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            assert_eq!(
                Libvirt.start(&vm)[0].display(),
                "virsh --connect qemu:///session start dev"
            );
            assert_eq!(
                Libvirt.destroy(&vm)[0].display(),
                "virsh --connect qemu:///session undefine dev --nvram"
            );
            assert_eq!(
                Libvirt
                    .screenshot(&vm, Path::new("/tmp/s.png"))
                    .unwrap()
                    .display(),
                "virsh --connect qemu:///session screenshot dev /tmp/s.png"
            );
            let endpoint = Libvirt.ssh_endpoint(&vm, &mut fake("")).unwrap();
            assert_eq!((endpoint.host.as_str(), endpoint.port), ("127.0.0.1", 2223));
            let mut runner = fake("other\ndev\n\n");
            assert_eq!(Libvirt.running(&vm, &mut runner), Some(true));
            assert_eq!(
                runner.0[0].display(),
                "virsh --connect qemu:///session list --name --state-running"
            );
            assert_eq!(Libvirt.running(&vm, &mut fake("dev2\n")), Some(false));
            assert_eq!(Utm.running(&vm, &mut fake("")), None);
        }

        // Phase: a_raw_image_is_the_backing_file_of_a_qcow2_child.
        {
            let dir = tempfile::tempdir().unwrap();
            let disk = dir.path().join("dev.qcow2").display().to_string();
            let entry = VmEntry {
                image: Some("/img/disk.raw".into()),
                image_format: Some("raw".into()),
                disk: Some(disk.clone()),
                ..entry()
            };
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            assert_eq!(
                Libvirt.create(&vm, KEY).unwrap()[0].display(),
                format!("qemu-img create -f qcow2 -F raw -b /img/disk.raw {disk}")
            );
            // The child is qcow2 whatever its backing file is.
            assert!(domain_xml(&vm, KEY).unwrap().contains("type='qcow2'"));
        }

        // Phase: a_boot_esp_is_attached_read_only_and_boots_first.
        {
            let with_esp = VmEntry {
                disk: Some(r"C:\vms\dev.qcow2".into()),
                boot_esp: Some(r"C:\esp\override.raw".into()),
                firmware_code: Some("code.fd".into()),
                ..entry()
            };
            let vm = Vm {
                name: "dev",
                entry: &with_esp,
            };
            let args = Whpx::qemu_args(&vm);
            let esp = args
                .iter()
                .position(|a| a.contains("id=bootesp"))
                .expect("ESP drive");
            assert!(args[esp].contains("readonly=on"), "{}", args[esp]);
            assert!(args[esp].contains("format=raw"), "{}", args[esp]);
            assert!(args[esp].ends_with(r"override.raw"), "{}", args[esp]);
            assert!(
                args.iter()
                    .any(|a| a == "virtio-blk-pci,drive=bootesp,bootindex=0")
            );
            assert!(
                args.iter()
                    .any(|a| a == "virtio-blk-pci,drive=osdisk,bootindex=1")
            );
            let without = VmEntry {
                boot_esp: None,
                ..with_esp.clone()
            };
            assert!(
                !Whpx::qemu_args(&Vm {
                    name: "dev",
                    entry: &without
                })
                .iter()
                .any(|a| a.contains("bootesp"))
            );

            let xml = domain_xml(&vm, KEY).unwrap();
            let esp_disk = xml.split("<disk").nth(2).expect("second disk");
            assert!(esp_disk.contains("<readonly/>"), "{xml}");
            assert!(esp_disk.contains("<boot order='1'/>"), "{xml}");
            assert!(esp_disk.contains("type='raw'"), "{xml}");
            let os_disk = xml.split("<disk").nth(1).unwrap();
            assert!(os_disk.contains("<boot order='2'/>"), "{xml}");
            assert!(!vm_xml(&entry()).contains("<boot order="));

            for name in ["libvirt", "whpx", "qemu", "utm", "hyperv"] {
                let supported = adapter(name).unwrap().capabilities().boot_esp;
                assert_eq!(supported, matches!(name, "libvirt" | "whpx"), "{name}");
            }
        }

        // Phase: removing_a_vm_unregisters_it_and_names_its_files.
        {
            let entry = entry();
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            assert_eq!(
                Libvirt
                    .destroy(&vm)
                    .iter()
                    .map(CommandLine::display)
                    .collect::<Vec<_>>(),
                ["virsh --connect qemu:///session undefine dev --nvram"]
            );
            assert!(Whpx.destroy(&vm).is_empty());
            assert!(
                Whpx.power_off(&vm)
                    .unwrap()
                    .args
                    .last()
                    .unwrap()
                    .contains("quit")
            );
            assert!(
                HyperV.destroy(&vm)[0]
                    .args
                    .last()
                    .unwrap()
                    .contains("Remove-VM")
            );
            assert!(
                HyperV
                    .power_off(&vm)
                    .unwrap()
                    .args
                    .last()
                    .unwrap()
                    .contains("-TurnOff")
            );
            assert_eq!(Utm.destroy(&vm)[0].display(), "utmctl delete dev");
            for name in ["libvirt", "qemu", "utm", "hyperv", "whpx"] {
                assert!(adapter(name).unwrap().power_off(&vm).is_some(), "{name}");
            }
            let files = vm_files("/vms/dev.qcow2");
            for file in [
                "/vms/dev.qcow2",
                "/vms/dev.qcow2.xml",
                "/vms/dev.qcow2.vars.fd",
                "/vms/dev.qcow2.pid",
                "/vms/dev.qcow2.ttyS0.log",
                "/vms/dev.qcow2.uart.log",
            ] {
                assert!(files.iter().any(|f| f == file), "{file} in {files:?}");
            }
        }

        // Phase: libvirt_boots_a_direct_kernel_from_the_registry_entry.
        {
            let dir = tempfile::tempdir().unwrap();
            let disk = dir.path().join("dev.qcow2").display().to_string();
            let mut entry = VmEntry {
                disk: Some(disk.clone()),
                ..entry()
            };
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            let plain = domain_xml(&vm, KEY).unwrap();
            assert!(!plain.contains("<kernel>"), "{plain}");
            entry.direct_kernel = Some(crate::config::DirectKernel {
                kernel: "/c/tilo-kernel/direct/dev/vmlinuz".into(),
                initrd: "/c/tilo-kernel/direct/dev/initrd.img".into(),
                cmdline: "root=UUID=1 ro console=ttyS0 a<b".into(),
                loader_partuuid: None,
                kver: "7.2.5+dev".into(),
            });
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            let xml = domain_xml(&vm, KEY).unwrap();
            let os = xml
                .split_once("<os ")
                .and_then(|(_, rest)| rest.split_once("</os>"))
                .map(|(os, _)| os)
                .unwrap();
            assert!(
                os.contains("<kernel>/c/tilo-kernel/direct/dev/vmlinuz</kernel>"),
                "{os}"
            );
            assert!(
                os.contains("<initrd>/c/tilo-kernel/direct/dev/initrd.img</initrd>"),
                "{os}"
            );
            assert!(
                os.contains("<cmdline>root=UUID=1 ro console=ttyS0 a&lt;b</cmdline>"),
                "{os}"
            );
            // Redefined under the existing domain's UUID, which virsh reports.
            let mut runner = fake("e7482d68-86d0-43dc-8f53-fc6b508ea038\n");
            let commands = Libvirt.redefine(&vm, KEY, &mut runner).unwrap();
            assert_eq!(
                runner.0[0].display(),
                "virsh --connect qemu:///session domuuid dev"
            );
            assert_eq!(
                commands[0].display(),
                format!("virsh --connect qemu:///session define {disk}.xml")
            );
            let written = std::fs::read_to_string(format!("{disk}.xml")).unwrap();
            assert!(written.contains("<kernel>"), "{written}");
            assert!(
                written.contains("<uuid>e7482d68-86d0-43dc-8f53-fc6b508ea038</uuid>"),
                "{written}"
            );
            assert_eq!(
                Libvirt.power_off(&vm).unwrap().display(),
                "virsh --connect qemu:///session destroy dev"
            );
        }
    }

    /// QEMU wraps `vm.sh`; disks use the qemu-img resolved at creation; UTM commands; Hyper-V commands quote
    /// for PowerShell; configured SSH wins; capabilities match the optional methods.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn other_hypervisors_quote_and_resolve_their_commands() {
        // Phase: qemu_wraps_vm_sh.
        {
            let entry = entry();
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            assert_eq!(
                Qemu.start(&vm)[0].display(),
                "TILO_SSH_PORT=2223 TILO_MEM=4096 TILO_SMP=2 bash os/dev/vm.sh boot --2d --disk /vms/dev.qcow2"
            );
            assert_eq!(
                Qemu.reset(&vm).unwrap().display(),
                "TILO_SSH_PORT=2223 bash os/dev/vm.sh cmd system_reset"
            );
            assert_eq!(
                Qemu.screenshot(&vm, Path::new("/tmp/out/boot.png"))
                    .unwrap()
                    .display(),
                "TILO_SSH_PORT=2223 TILO_RUN=/tmp/out bash os/dev/vm.sh shot boot"
            );
            assert!(Qemu.destroy(&vm).is_empty());
            assert_eq!(
                Qemu.power_off(&vm).unwrap().display(),
                "TILO_SSH_PORT=2223 bash os/dev/vm.sh stop"
            );
        }

        // Phase: disks_are_made_with_the_qemu_img_resolved_at_creation.
        {
            let dir = tempfile::tempdir().unwrap();
            let entry = VmEntry {
                qemu_img: Some("/usr/bin/qemu-img".into()),
                disk: Some(dir.path().join("dev.qcow2").display().to_string()),
                ..entry()
            };
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            assert!(
                Libvirt.create(&vm, KEY).unwrap()[0]
                    .display()
                    .starts_with("/usr/bin/qemu-img create")
            );
            assert!(
                Qemu.create(&vm, KEY).unwrap()[0]
                    .display()
                    .starts_with("/usr/bin/qemu-img create")
            );
        }

        // Phase: utm_commands.
        {
            let entry = VmEntry {
                template: Some("Tilo Dev".into()),
                ..entry()
            };
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            assert_eq!(
                Utm.create(&vm, KEY).unwrap()[0].display(),
                "utmctl clone 'Tilo Dev' --name dev"
            );
            assert_eq!(Utm.stop(&vm)[0].display(), "utmctl stop dev --request");
            assert_eq!(Utm.serial(&vm).unwrap().display(), "utmctl attach dev");
            let mut runner = fake("fe80::1\n192.168.64.5\n");
            let endpoint = Utm.ssh_endpoint(&vm, &mut runner).unwrap();
            assert_eq!(endpoint.host, "192.168.64.5");
            assert_eq!(runner.0[0].display(), "utmctl ip-address dev");
            let no_template = entry_without_template();
            assert!(matches!(
                Utm.create(
                    &Vm {
                        name: "dev",
                        entry: &no_template
                    },
                    KEY
                ),
                Err(CliError::Config { .. })
            ));
        }

        // Phase: hyperv_commands_quote_for_powershell.
        {
            let entry = VmEntry {
                disk: Some(r"C:\VMs\it's.vhdx".into()),
                image: None,
                ..entry()
            };
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            let create = HyperV.create(&vm, KEY).unwrap();
            assert_eq!(create.len(), 1);
            assert_eq!(create[0].program, "powershell.exe");
            assert_eq!(
                create[0].args[..3],
                ["-NoProfile", "-NonInteractive", "-Command"]
            );
            let script = &create[0].args[3];
            assert!(script.contains(r"-VHDPath 'C:\VMs\it''s.vhdx'"), "{script}");
            assert!(script.contains("-MemoryStartupBytes 4096MB"));
            assert!(
                script.contains(
                    r"Set-VMComPort -VMName 'dev' -Number 1 -Path '\\.\pipe\tilo-vm-dev'"
                )
            );
            assert_eq!(HyperV.start(&vm)[0].args[3], "Start-VM -Name 'dev'");
            assert_eq!(
                HyperV.reset(&vm).unwrap().args[3],
                "Restart-VM -Name 'dev' -Force"
            );
            assert_eq!(
                HyperV.serial(&vm).unwrap().display(),
                r"plink.exe -serial \\.\pipe\tilo-vm-dev"
            );
            let endpoint = HyperV.ssh_endpoint(&vm, &mut fake("172.20.1.9\n")).unwrap();
            assert_eq!(endpoint.host, "172.20.1.9");
        }

        // Phase: configured_ssh_wins.
        {
            let entry = VmEntry {
                ssh: Some("tilo@10.0.0.2:2200".into()),
                ..entry()
            };
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            let mut runner = fake("");
            let endpoint = Utm.ssh_endpoint(&vm, &mut runner).unwrap();
            assert_eq!(
                endpoint,
                SshEndpoint {
                    user: "tilo".into(),
                    host: "10.0.0.2".into(),
                    port: 2200
                }
            );
            assert!(
                runner.0.is_empty(),
                "no query when the address is configured"
            );
            assert_eq!(SshEndpoint::parse("vm.local").unwrap().port, 22);
            assert!(SshEndpoint::parse("@x").is_none());
        }

        // Phase: capabilities_match_the_optional_methods.
        {
            let dir = tempfile::tempdir().unwrap();
            let disk = dir.path().join("dev.qcow2").display().to_string();
            let entry = VmEntry {
                template: Some("Tilo Dev".into()),
                disk: Some(disk.clone()),
                ..entry()
            };
            let vm = Vm {
                name: "dev",
                entry: &entry,
            };
            for name in ["libvirt", "qemu", "utm", "hyperv"] {
                let a = adapter(name).unwrap();
                let caps = a.capabilities();
                assert_eq!(caps.serial, a.serial(&vm).is_some(), "{name} serial");
                assert_eq!(
                    caps.screenshot,
                    a.screenshot(&vm, Path::new("/tmp/x.png")).is_some(),
                    "{name} shot"
                );
                assert_eq!(caps.reset, a.reset(&vm).is_some(), "{name} reset");
                let _ = std::fs::remove_file(domain_xml_path(&disk));
                let in_args = a.create(&vm, KEY).unwrap().iter().any(|c| {
                    c.args
                        .iter()
                        .any(|arg| arg.contains("io.systemd.credential"))
                });
                let in_domain = std::fs::read_to_string(domain_xml_path(&disk))
                    .is_ok_and(|xml| xml.contains("io.systemd.credential"));
                let passes_key = in_args || in_domain;
                assert_eq!(caps.credentials.is_some(), passes_key, "{name} credentials");
                assert_eq!(
                    caps.control.is_some(),
                    a.control(&vm, &[]).is_some(),
                    "{name} control"
                );
                assert_eq!(
                    caps.direct_kernel,
                    a.redefine(&vm, KEY, &mut fake("")).is_ok() && a.power_off(&vm).is_some(),
                    "{name} direct kernel"
                );
            }
            assert!(adapter("vbox").is_err());
        }
    }

    fn entry_without_template() -> VmEntry {
        entry()
    }

    #[test]
    fn libvirt_video_has_the_requested_heads_and_first_mode() {
        let plain = domain_xml(
            &Vm {
                name: "dev",
                entry: &entry(),
            },
            KEY,
        )
        .unwrap();
        assert!(
            plain.contains("<video><model type='virtio'/></video>"),
            "{plain}"
        );
        let dual = VmEntry {
            heads: Some(2),
            resolution: Some("5120x2880".into()),
            ..entry()
        };
        let xml = domain_xml(
            &Vm {
                name: "dev",
                entry: &dual,
            },
            KEY,
        )
        .unwrap();
        assert!(
            xml.contains(
                "<video><model type='virtio' heads='2'><resolution x='5120' y='2880'/></model></video>"
            ),
            "{xml}"
        );
        assert!(
            !xml.contains("egl-headless") && !xml.contains("accel3d"),
            "{xml}"
        );
        let gl = VmEntry {
            gl: true,
            resolution: Some("5120x2880".into()),
            ..entry()
        };
        let xml = domain_xml(
            &Vm {
                name: "dev",
                entry: &gl,
            },
            KEY,
        )
        .unwrap();
        assert!(
            xml.contains(
                "<graphics type='spice' autoport='yes'/>\n    <graphics type='egl-headless'/>"
            ),
            "{xml}"
        );
        assert!(
            xml.contains(
                "<video><model type='virtio'><acceleration accel3d='yes'/><resolution x='5120' y='2880'/></model></video>"
            ),
            "{xml}"
        );
    }
}
