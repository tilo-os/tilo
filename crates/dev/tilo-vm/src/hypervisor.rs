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
}

/// What an adapter can do beyond the required five operations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub serial: bool,
    pub screenshot: bool,
    pub reset: bool,
    /// Native control channel passed through by `vm control`.
    pub control: Option<&'static str>,
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
        self.entry.memory_mb.unwrap_or(8192)
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
    /// Commands that create the VM from its image.
    ///
    /// # Errors
    /// Missing registry fields.
    fn create(&self, vm: &Vm<'_>) -> Result<Vec<CommandLine>, CliError>;
    fn start(&self, vm: &Vm<'_>) -> Vec<CommandLine>;
    fn stop(&self, vm: &Vm<'_>) -> Vec<CommandLine>;
    fn destroy(&self, vm: &Vm<'_>) -> Vec<CommandLine>;
    /// Where SSH listens; may query the hypervisor.
    ///
    /// # Errors
    /// The hypervisor could not report an address.
    fn ssh_endpoint(&self, vm: &Vm<'_>, runner: &mut dyn Runner) -> Result<SshEndpoint, CliError>;

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
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
    /// Host tools this adapter needs, for `doctor`.
    fn tools(&self) -> &'static [&'static str];
}

/// # Errors
/// Unknown hypervisor names.
pub fn adapter(name: &str) -> Result<Box<dyn Hypervisor>, CliError> {
    match name {
        "libvirt" => Ok(Box::new(Libvirt)),
        "qemu" => Ok(Box::new(Qemu)),
        "utm" => Ok(Box::new(Utm)),
        "hyperv" => Ok(Box::new(HyperV)),
        other => Err(CliError::Unsupported {
            what: format!("hypervisor '{other}'"),
            alternative: "set `hypervisor` to libvirt, qemu, utm or hyperv in vms.toml".into(),
        }),
    }
}

/// The adapter a new VM gets on this host by default.
pub fn native() -> &'static str {
    if cfg!(target_os = "macos") {
        "utm"
    } else if cfg!(windows) {
        "hyperv"
    } else {
        "libvirt"
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

/// A qcow2 child of the published image, so the image stays pristine.
fn overlay_disk(vm: &Vm<'_>) -> Result<Option<CommandLine>, CliError> {
    let disk = vm.disk()?;
    Ok(vm.entry.image.as_deref().map(|image| {
        CommandLine::new(
            "qemu-img",
            &["create", "-f", "qcow2", "-F", "qcow2", "-b", image, disk],
        )
    }))
}

// ---- libvirt (Linux) ----------------------------------------------------------

pub struct Libvirt;

impl Libvirt {
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

    fn create(&self, vm: &Vm<'_>) -> Result<Vec<CommandLine>, CliError> {
        let mut commands: Vec<CommandLine> = overlay_disk(vm)?.into_iter().collect();
        let mut install = CommandLine::new(
            "virt-install",
            &[
                "--connect",
                vm.entry.uri.as_deref().unwrap_or("qemu:///session"),
                "--name",
                vm.name,
            ],
        )
        .arg("--memory")
        .arg(vm.memory().to_string())
        .arg("--vcpus")
        .arg(vm.cpus().to_string())
        .args_from(&[
            "--import",
            "--osinfo",
            "detect=on,require=off",
            "--boot",
            "uefi",
        ])
        .arg("--disk")
        .arg(format!("path={},format=qcow2,bus=virtio", vm.disk()?))
        // Provisional: passt port forwarding needs libvirt >= 9.2 and virt-install >= 4.1.
        .arg("--network")
        .arg(format!("passt,portForward={}:22", vm.ssh_port()))
        .args_from(&[
            "--graphics",
            "spice",
            "--video",
            "virtio",
            "--serial",
            "pty",
            "--noautoconsole",
            "--noreboot",
        ]);
        if let Some(arch) = &vm.entry.arch {
            install = install.arg("--arch").arg(arch.clone());
            if vm.entry.emulated {
                install = install.args_from(&["--virt-type", "qemu"]);
            }
        }
        commands.push(install);
        Ok(commands)
    }

    fn start(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![Self::virsh(vm, &["start", vm.name])]
    }

    fn stop(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![Self::virsh(vm, &["shutdown", vm.name])]
    }

    fn destroy(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![
            Self::virsh(vm, &["destroy", vm.name]),
            Self::virsh(vm, &["undefine", vm.name, "--nvram"]),
        ]
    }

    fn ssh_endpoint(&self, vm: &Vm<'_>, _runner: &mut dyn Runner) -> Result<SshEndpoint, CliError> {
        Ok(configured_endpoint(vm, "127.0.0.1", vm.ssh_port()))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            serial: true,
            screenshot: true,
            reset: true,
            control: Some("virsh"),
        }
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

    fn tools(&self) -> &'static [&'static str] {
        &["virsh", "virt-install", "qemu-img"]
    }
}

// ---- raw QEMU via os/dev/vm.sh (Linux, CI and scenario tests) ---------------

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

    fn create(&self, vm: &Vm<'_>) -> Result<Vec<CommandLine>, CliError> {
        Ok(overlay_disk(vm)?.into_iter().collect())
    }

    fn start(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        let disk = vm.entry.disk.clone().unwrap_or_default();
        vec![Self::script(vm, &["boot", "--gl", "--disk", &disk])]
    }

    fn stop(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        vec![Self::script(vm, &["shutdown"])]
    }

    fn destroy(&self, vm: &Vm<'_>) -> Vec<CommandLine> {
        let mut commands = vec![Self::script(vm, &["stop"])];
        if let Some(disk) = &vm.entry.disk {
            commands.push(CommandLine::new("rm", &["-f", disk]));
        }
        commands
    }

    fn ssh_endpoint(&self, vm: &Vm<'_>, _runner: &mut dyn Runner) -> Result<SshEndpoint, CliError> {
        Ok(configured_endpoint(vm, "127.0.0.1", vm.ssh_port()))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            serial: true,
            screenshot: true,
            reset: true,
            control: Some("QEMU monitor (HMP)"),
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

    fn tools(&self) -> &'static [&'static str] {
        &["bash", "qemu-img"]
    }
}

// ---- UTM (macOS) --------------------------------------------------------------

pub struct Utm;

impl Hypervisor for Utm {
    fn name(&self) -> &'static str {
        "utm"
    }

    fn create(&self, vm: &Vm<'_>) -> Result<Vec<CommandLine>, CliError> {
        // utmctl cannot import a disk; a human imports the published image
        // once as a template VM in UTM, and each dev VM is a clone of it.
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
        vec![
            CommandLine::new("utmctl", &["stop", vm.name, "--kill"]),
            CommandLine::new("utmctl", &["delete", vm.name]),
        ]
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
            .find(|l| !l.is_empty() && !l.contains(':'));
        match (output.success, host) {
            (true, Some(host)) => Ok(SshEndpoint {
                user: "root".into(),
                host: host.into(),
                port: 22,
            }),
            _ => Err(CliError::Unreachable {
                vm: vm.name.into(),
                detail: format!("`{}` reported no IPv4 address", command.display()),
                debug: vec![format!("tilo-vm vm serial {}", vm.name)],
            }),
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            serial: true,
            screenshot: false,
            reset: false,
            control: Some("utmctl"),
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

    fn tools(&self) -> &'static [&'static str] {
        &["utmctl"]
    }
}

// ---- Hyper-V (Windows) ----------------------------------------------------------

pub struct HyperV;

/// A PowerShell single-quoted string literal.
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

    fn create(&self, vm: &Vm<'_>) -> Result<Vec<CommandLine>, CliError> {
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
        vec![powershell(&format!(
            "Stop-VM -Name {name} -TurnOff -Force; Remove-VM -Name {name} -Force"
        ))]
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
            control: Some("PowerShell (Hyper-V module)"),
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

    fn tools(&self) -> &'static [&'static str] {
        &["powershell.exe"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn libvirt_commands() {
        let entry = entry();
        let vm = Vm {
            name: "dev",
            entry: &entry,
        };
        let create = Libvirt.create(&vm).unwrap();
        assert_eq!(
            create[0].display(),
            "qemu-img create -f qcow2 -F qcow2 -b /img/tilo-vm.qcow2 /vms/dev.qcow2"
        );
        let install = create[1].display();
        assert!(install.starts_with(
            "virt-install --connect qemu:///session --name dev --memory 4096 --vcpus 2 --import"
        ));
        assert!(install.contains("--disk path=/vms/dev.qcow2,format=qcow2,bus=virtio"));
        assert!(install.contains("--network passt,portForward=2223:22"));
        assert!(!install.contains("--arch"));
        assert_eq!(
            Libvirt.start(&vm)[0].display(),
            "virsh --connect qemu:///session start dev"
        );
        assert_eq!(
            Libvirt.destroy(&vm)[1].display(),
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
    }

    #[test]
    fn libvirt_emulation_is_explicit() {
        let entry = VmEntry {
            arch: Some("aarch64".into()),
            emulated: true,
            ..entry()
        };
        let install = Libvirt
            .create(&Vm {
                name: "arm",
                entry: &entry,
            })
            .unwrap()[1]
            .display();
        assert!(install.contains("--arch aarch64 --virt-type qemu"));
    }

    #[test]
    fn qemu_wraps_vm_sh() {
        let entry = entry();
        let vm = Vm {
            name: "dev",
            entry: &entry,
        };
        assert_eq!(
            Qemu.start(&vm)[0].display(),
            "TILO_SSH_PORT=2223 bash os/dev/vm.sh boot --gl --disk /vms/dev.qcow2"
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
        assert_eq!(Qemu.destroy(&vm)[1].display(), "rm -f /vms/dev.qcow2");
    }

    #[test]
    fn utm_commands() {
        let entry = VmEntry {
            template: Some("Tilo Dev".into()),
            ..entry()
        };
        let vm = Vm {
            name: "dev",
            entry: &entry,
        };
        assert_eq!(
            Utm.create(&vm).unwrap()[0].display(),
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
            Utm.create(&Vm {
                name: "dev",
                entry: &no_template
            }),
            Err(CliError::Config { .. })
        ));
    }

    fn entry_without_template() -> VmEntry {
        entry()
    }

    #[test]
    fn hyperv_commands_quote_for_powershell() {
        let entry = VmEntry {
            disk: Some(r"C:\VMs\it's.vhdx".into()),
            image: None,
            ..entry()
        };
        let vm = Vm {
            name: "dev",
            entry: &entry,
        };
        let create = HyperV.create(&vm).unwrap();
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
            script.contains(r"Set-VMComPort -VMName 'dev' -Number 1 -Path '\\.\pipe\tilo-vm-dev'")
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

    #[test]
    fn configured_ssh_wins() {
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

    #[test]
    fn capabilities_match_the_optional_methods() {
        let entry = entry();
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
            assert_eq!(
                caps.control.is_some(),
                a.control(&vm, &[]).is_some(),
                "{name} control"
            );
        }
        assert!(adapter("vbox").is_err());
    }
}
