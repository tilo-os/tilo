use std::ffi::OsString;
use std::path::Path;

use libvirt_domain::Connection;
pub use libvirt_domain::DomainState;
use thiserror::Error;

use crate::{
    APPLIANCE_DOMAIN_NAME, ApplianceConfig, CommandError, CommandOutput, CommandRequest,
    CommandRunner, ConfigError, CpuMode, FirmwareConfig, GraphicsAssignment,
};

const LIBVIRT_URI: &str = Connection::System.uri();
const METADATA_MARKER: &str = "<tilo:windows-appliance xmlns:tilo='https://tilo.sh/vm/1'/>";
const METADATA_ELEMENT: &str = "<tilo:windows-appliance";
const INSTALL_MEDIA: &str = "/var/lib/tilo/windows/install-media.iso";
const GUEST_TOOLS_MEDIA: &str = "/var/lib/tilo/windows/tilo-guest-tools.iso";
/// Fedora's virtio-win package is shipped by Tilo as a host-owned, read-only
/// setup aid. It is detached before the appliance becomes launchable.
const VIRTIO_DRIVERS_MEDIA: &str = "/usr/share/virtio-win/virtio-win.iso";
/// The second NIC is never present in the persistent domain definition. It is
/// attached live only while an authenticated, purpose-bound egress lease is
/// active. Keeping a separate MAC makes cleanup deterministic after a retry.
const EGRESS_MAC: &str = "52:54:00:71:4f:51";
const EGRESS_NETWORK: &str = "tilo-windows-egress";
const MAX_EGRESS_SECONDS: u64 = 30 * 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EgressPurpose {
    Activation,
    WindowsUpdate,
}

impl EgressPurpose {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Activation => "activation",
            Self::WindowsUpdate => "windows_update",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EgressLease {
    pub id: String,
    pub purpose: EgressPurpose,
    pub expires_unix_seconds: u64,
}

pub trait VmBackend: Send + Sync {
    /// Defines the singleton if absent and rejects a same-named foreign domain.
    fn ensure_singleton(&self, config: &ApplianceConfig) -> Result<(), VmBackendError>;
    fn state(&self, config: &ApplianceConfig) -> Result<DomainState, VmBackendError>;
    fn start(&self, config: &ApplianceConfig) -> Result<(), VmBackendError>;
    /// Graceful ACPI shutdown. Repeated calls are idempotent and never turn a
    /// paused or foreign domain into a running one.
    fn shutdown(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        let _ = config;
        Err(VmBackendError::UnsupportedAction("shutdown"))
    }
    /// Forced power-off, intended only after a bounded graceful wait.
    fn force_shutdown(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        let _ = config;
        Err(VmBackendError::UnsupportedAction("force_shutdown"))
    }
    fn restart(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        let _ = config;
        Err(VmBackendError::UnsupportedAction("restart"))
    }
    fn suspend(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        let _ = config;
        Err(VmBackendError::UnsupportedAction("suspend"))
    }
    fn resume(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        let _ = config;
        Err(VmBackendError::UnsupportedAction("resume"))
    }
    /// Attach a NAT-backed interface for a bounded operation. The normal
    /// domain always retains only its host-only interface.
    fn open_egress(
        &self,
        config: &ApplianceConfig,
        purpose: EgressPurpose,
        now_unix_seconds: u64,
        ttl_seconds: u64,
    ) -> Result<EgressLease, VmBackendError> {
        let _ = (config, purpose, now_unix_seconds, ttl_seconds);
        Err(VmBackendError::UnsupportedAction("open_egress"))
    }
    fn close_egress(
        &self,
        config: &ApplianceConfig,
        lease: &EgressLease,
    ) -> Result<(), VmBackendError> {
        let _ = (config, lease);
        Err(VmBackendError::UnsupportedAction("close_egress"))
    }
    /// Permanently removes setup media after authenticated guest enrollment.
    /// Repeated calls must be harmless.
    fn retire_installation_media(&self, config: &ApplianceConfig) -> Result<(), VmBackendError>;
}

#[derive(Clone, Debug)]
pub struct LibvirtBackend<R> {
    runner: R,
    virsh: OsString,
}

impl<R> LibvirtBackend<R> {
    #[must_use]
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            virsh: OsString::from("virsh"),
        }
    }
}

impl<R: CommandRunner> LibvirtBackend<R> {
    fn virsh<I, S>(&self, args: I) -> Result<CommandOutput, VmBackendError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let request = CommandRequest::new(self.virsh.clone())
            .timeout(std::time::Duration::from_secs(300))
            .args(["--connect", LIBVIRT_URI])
            .args(args);
        self.runner.run(&request).map_err(Into::into)
    }

    fn successful<I, S>(&self, args: I) -> Result<CommandOutput, VmBackendError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let output = self.virsh(args)?;
        if output.success {
            Ok(output)
        } else {
            Err(VmBackendError::Virsh {
                code: output.code,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }

    /// Persistently attach one already-prepared GPU partition. The VM must be
    /// off; the caller has separately proved this exact VF/mdev through the
    /// read-only capability probe and an owner-authorised boundary.
    pub fn attach_graphics(
        &self,
        config: &ApplianceConfig,
        assignment: &GraphicsAssignment,
    ) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        if self.state(config)? != DomainState::ShutOff {
            return Err(VmBackendError::GraphicsRequiresShutoff);
        }
        let xml = assignment
            .domain_xml()
            .ok_or(VmBackendError::InvalidGraphicsAssignment)?;
        let request = CommandRequest::new(self.virsh.clone())
            .timeout(std::time::Duration::from_secs(300))
            .args([
                "--connect",
                LIBVIRT_URI,
                "attach-device",
                APPLIANCE_DOMAIN_NAME,
                "/dev/stdin",
                "--config",
            ])
            .stdin_secret(xml.as_bytes());
        let output = self.runner.run(&request)?;
        if output.success {
            return Ok(());
        }
        Err(VmBackendError::Virsh {
            code: output.code,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }

    /// Detach only the exact partition recorded by Tilo at attachment time.
    pub fn detach_graphics(
        &self,
        config: &ApplianceConfig,
        assignment: &GraphicsAssignment,
    ) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        if self.state(config)? != DomainState::ShutOff {
            return Err(VmBackendError::GraphicsRequiresShutoff);
        }
        let xml = assignment
            .domain_xml()
            .ok_or(VmBackendError::InvalidGraphicsAssignment)?;
        let request = CommandRequest::new(self.virsh.clone())
            .timeout(std::time::Duration::from_secs(300))
            .args([
                "--connect",
                LIBVIRT_URI,
                "detach-device",
                APPLIANCE_DOMAIN_NAME,
                "/dev/stdin",
                "--config",
            ])
            .stdin_secret(xml.as_bytes());
        let output = self.runner.run(&request)?;
        if output.success {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_ascii_lowercase();
        if stderr.contains("not found") || stderr.contains("no matching") {
            return Ok(());
        }
        Err(VmBackendError::Virsh {
            code: output.code,
            stderr,
        })
    }
}

impl<R: CommandRunner> VmBackend for LibvirtBackend<R> {
    fn ensure_singleton(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        config.validate()?;
        let domains = self.successful(["list", "--all", "--name"])?;
        let names = String::from_utf8(domains.stdout)?;
        let exists = names
            .lines()
            .any(|name| name.trim() == APPLIANCE_DOMAIN_NAME);
        if exists {
            let xml = self.successful(["dumpxml", APPLIANCE_DOMAIN_NAME])?;
            let xml = String::from_utf8(xml.stdout)?;
            let expected_uuid = format!("<uuid>{}</uuid>", config.domain_uuid);
            let expected_smbios_serial =
                format!("<entry name='serial'>{}</entry>", config.domain_uuid);
            let expected_smbios_uuid = format!("<entry name='uuid'>{}</entry>", config.domain_uuid);
            if !xml.contains(&expected_uuid)
                || !xml.contains(&expected_smbios_serial)
                || !xml.contains(&expected_smbios_uuid)
                || !xml.contains("<serial>tilo-windows-system</serial>")
                || !xml.contains("<mac address='52:54:00:71:4f:53'/>")
                || !xml.contains(METADATA_ELEMENT)
            {
                return Err(VmBackendError::ForeignDomainCollision);
            }
            return Ok(());
        }

        let xml = render_domain_xml(config)?;
        let request = CommandRequest::new(self.virsh.clone())
            .timeout(std::time::Duration::from_secs(300))
            .args(["--connect", LIBVIRT_URI, "define", "/dev/stdin"])
            .stdin_secret(xml.as_bytes());
        let output = self.runner.run(&request)?;
        if !output.success {
            return Err(VmBackendError::Virsh {
                code: output.code,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(())
    }

    fn state(&self, config: &ApplianceConfig) -> Result<DomainState, VmBackendError> {
        config.validate()?;
        let domains = self.successful(["list", "--all", "--name"])?;
        let names = String::from_utf8(domains.stdout)?;
        if !DomainState::listed(&names, &config.domain_name) {
            return Ok(DomainState::Absent);
        }
        let state = self.successful(["domstate", APPLIANCE_DOMAIN_NAME])?;
        Ok(DomainState::from_domstate(&String::from_utf8(
            state.stdout,
        )?))
    }

    fn start(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        match self.state(config)? {
            DomainState::Running => Ok(()),
            DomainState::ShutOff => self
                .successful(["start", APPLIANCE_DOMAIN_NAME])
                .map(|_| ()),
            DomainState::Absent => Err(VmBackendError::DomainDisappeared),
            DomainState::Paused
            | DomainState::Suspended
            | DomainState::Starting
            | DomainState::ShuttingDown
            | DomainState::Other => Err(VmBackendError::UnsafeDomainState),
            DomainState::Crashed => self
                .successful(["start", APPLIANCE_DOMAIN_NAME])
                .map(|_| ()),
        }
    }

    fn shutdown(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        match self.state(config)? {
            DomainState::Absent
            | DomainState::ShutOff
            | DomainState::ShuttingDown
            | DomainState::Crashed => Ok(()),
            DomainState::Running => self
                .successful(["shutdown", APPLIANCE_DOMAIN_NAME])
                .map(|_| ()),
            DomainState::Paused
            | DomainState::Suspended
            | DomainState::Starting
            | DomainState::Other => Err(VmBackendError::UnsafeDomainState),
        }
    }

    fn force_shutdown(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        match self.state(config)? {
            DomainState::Absent | DomainState::ShutOff | DomainState::Crashed => Ok(()),
            DomainState::Running
            | DomainState::ShuttingDown
            | DomainState::Starting
            | DomainState::Paused
            | DomainState::Suspended => self
                .successful(["destroy", APPLIANCE_DOMAIN_NAME])
                .map(|_| ()),
            DomainState::Other => Err(VmBackendError::UnsafeDomainState),
        }
    }

    fn restart(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        match self.state(config)? {
            DomainState::ShutOff | DomainState::Absent | DomainState::Crashed => self.start(config),
            DomainState::Running => self
                .successful(["reboot", APPLIANCE_DOMAIN_NAME, "--mode", "acpi"])
                .map(|_| ()),
            DomainState::ShuttingDown | DomainState::Starting => Ok(()),
            DomainState::Paused | DomainState::Suspended | DomainState::Other => {
                Err(VmBackendError::UnsafeDomainState)
            }
        }
    }

    fn suspend(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        match self.state(config)? {
            DomainState::Paused | DomainState::Suspended => Ok(()),
            DomainState::Running => self
                .successful(["suspend", APPLIANCE_DOMAIN_NAME])
                .map(|_| ()),
            _ => Err(VmBackendError::UnsafeDomainState),
        }
    }

    fn resume(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        match self.state(config)? {
            DomainState::Running => Ok(()),
            DomainState::Paused | DomainState::Suspended => self
                .successful(["resume", APPLIANCE_DOMAIN_NAME])
                .map(|_| ()),
            _ => Err(VmBackendError::UnsafeDomainState),
        }
    }

    fn open_egress(
        &self,
        config: &ApplianceConfig,
        purpose: EgressPurpose,
        now_unix_seconds: u64,
        ttl_seconds: u64,
    ) -> Result<EgressLease, VmBackendError> {
        self.ensure_singleton(config)?;
        if ttl_seconds == 0 || ttl_seconds > MAX_EGRESS_SECONDS {
            return Err(VmBackendError::InvalidEgressTtl);
        }
        if self.state(config)? != DomainState::Running {
            return Err(VmBackendError::UnsafeDomainState);
        }
        // Live-only attachment is deliberately not `--config`: a crash or
        // restart cannot accidentally make egress persistent.
        self.successful([
            "attach-interface",
            APPLIANCE_DOMAIN_NAME,
            "network",
            EGRESS_NETWORK,
            "--model",
            "virtio",
            "--mac",
            EGRESS_MAC,
            "--live",
        ])?;
        let id = format!("egress-{}-{}", purpose.as_str(), now_unix_seconds);
        Ok(EgressLease {
            id,
            purpose,
            expires_unix_seconds: now_unix_seconds.saturating_add(ttl_seconds),
        })
    }

    fn close_egress(
        &self,
        config: &ApplianceConfig,
        lease: &EgressLease,
    ) -> Result<(), VmBackendError> {
        config.validate()?;
        if !lease.id.starts_with("egress-") {
            return Err(VmBackendError::InvalidEgressLease);
        }
        // `--live` means cleanup does not alter the persistent default-deny
        // domain. libvirt reports an absent device as an error; treat that as
        // success to make timeout/retry cleanup safe.
        let output = self.virsh([
            "detach-interface",
            APPLIANCE_DOMAIN_NAME,
            "network",
            "--mac",
            EGRESS_MAC,
            "--live",
        ])?;
        if !output.success {
            let error = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            if error.contains("not found")
                || error.contains("no matching")
                || error.contains("detached")
            {
                return Ok(());
            }
            return Err(VmBackendError::Virsh {
                code: output.code,
                stderr: error,
            });
        }
        Ok(())
    }

    fn retire_installation_media(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        config.validate()?;
        for target in ["sdb", "sdc", "sdd"] {
            let xml = self.successful(["dumpxml", APPLIANCE_DOMAIN_NAME])?;
            let xml = String::from_utf8(xml.stdout)?;
            let single_quoted = format!("dev='{target}'");
            let double_quoted = format!("dev=\"{target}\"");
            if !xml.contains(&single_quoted) && !xml.contains(&double_quoted) {
                continue;
            }
            self.successful([
                "detach-disk",
                APPLIANCE_DOMAIN_NAME,
                target,
                "--live",
                "--config",
            ])?;
        }
        Ok(())
    }
}

/// Builds a headless, local-only Windows 11 domain. There is intentionally no
/// host directory device, USB redirection, bridged NIC, TCP graphics listener,
/// or clipboard device. The SPICE graphics device has no listener. Its agent
/// channel permits app-focused resize/cursor integration when the open guest
/// agent is installed, while libvirt blocks clipboard and file transfer at
/// the server even if a guest agent tries to advertise them.
pub fn render_domain_xml(config: &ApplianceConfig) -> Result<String, VmBackendError> {
    config.validate()?;
    let disk = xml_path(&config.system_disk)?;
    let (arch, machine, emulator, tpm, firmware, platform_features, clock) = match &config.firmware
    {
        FirmwareConfig::X86Pflash {
            secure_boot_loader,
            nvram_template,
            nvram,
        } => (
            "x86_64",
            "q35",
            "/usr/bin/qemu-system-x86_64",
            "tpm-crb",
            format!(
                "<loader readonly='yes' secure='yes' type='pflash'>{}</loader><nvram template='{}'>{}</nvram>",
                xml_path(secure_boot_loader)?,
                xml_path(nvram_template)?,
                xml_path(nvram)?
            ),
            "<apic/><hyperv mode='custom'><relaxed state='on'/><vapic state='on'/><spinlocks state='on' retries='8191'/><vpindex state='on'/><runtime state='on'/><synic state='on'/><stimer state='on'><direct state='on'/></stimer><frequencies state='on'/><tlbflush state='on'/><ipi state='on'/><evmcs state='on'/></hyperv><vmport state='off'/><smm state='on'/>",
            "<clock offset='localtime'><timer name='rtc' tickpolicy='catchup'/><timer name='pit' tickpolicy='delay'/><timer name='hpet' present='no'/><timer name='hypervclock' present='yes'/></clock>",
        ),
        FirmwareConfig::Arm64Variables { varstore } => (
            "aarch64",
            "virt",
            "/usr/bin/qemu-system-aarch64",
            "tpm-tis",
            format!(
                "<firmware><feature enabled='yes' name='secure-boot'/><feature enabled='yes' name='enrolled-keys'/></firmware><varstore path='{}'/>",
                xml_path(varstore)?
            ),
            "<gic version='3'/>",
            "<clock offset='utc'/>",
        ),
    };
    // ARM virt has no ACPI S3/S4 support. Libvirt rejects even explicit
    // disabled declarations there; keep them only on the x86 machine.
    let power_management = match &config.firmware {
        FirmwareConfig::X86Pflash { .. } => {
            "<pm><suspend-to-mem enabled='no'/><suspend-to-disk enabled='no'/></pm>"
        }
        FirmwareConfig::Arm64Variables { .. } => "",
    };
    let network = quick_xml::escape::escape(&config.host_only_network);
    let cpu_attributes = match config.performance.cpu_mode {
        CpuMode::HostPassthrough => "mode='host-passthrough' check='none' migratable='off'",
        CpuMode::HostModel => "mode='host-model' check='partial'",
    };
    // Keep the appliance within Windows client socket limits. Without an
    // explicit topology, libvirt can expose each allocated vCPU as a socket.
    let vsock = libvirt_domain::vsock_device_xml(Some(config.guest_cid));
    let cpu = format!(
        "<cpu {cpu_attributes}><topology sockets='1' cores='{}' threads='1'/></cpu>",
        config.performance.vcpus,
    );
    Ok(format!(
        "<domain type='kvm'>\
<name>{}</name><uuid>{}</uuid>\
<metadata>{METADATA_MARKER}</metadata>\
<!-- Stable SMBIOS identity is part of the activation device identity. -->\
<sysinfo type='smbios'><system><entry name='manufacturer'>Tilo</entry>\
<entry name='product'>Windows compatibility appliance</entry><entry name='serial'>{}</entry>\
<entry name='uuid'>{}</entry></system></sysinfo>\
<memory unit='MiB'>{}</memory><currentMemory unit='MiB'>{}</currentMemory>\
<vcpu placement='static'>{}</vcpu><iothreads>{}</iothreads>{cpu}\
<features><acpi/>{platform_features}</features>{clock}\
<os firmware='efi'><type arch='{arch}' machine='{machine}'>hvm</type>\
{firmware}<smbios mode='sysinfo'/><boot dev='hd'/><boot dev='cdrom'/></os>\
{power_management}\
<devices><emulator>{emulator}</emulator>\
<controller type='sata' index='0'/>\
<controller type='scsi' index='0' model='virtio-scsi'><driver iothread='1'/></controller>\
<controller type='virtio-serial' index='0'/>\
<disk type='file' device='disk'><driver name='qemu' type='qcow2' cache='none' io='native' discard='unmap' detect_zeroes='unmap'/>\
<source file='{disk}'/><target dev='sda' bus='scsi'/><serial>tilo-windows-system</serial></disk>\
<!-- Setup media use Windows-inbox SATA; only the installed system disk uses virtio-scsi. -->\
<disk type='file' device='cdrom'><driver name='qemu' type='raw'/><source file='{INSTALL_MEDIA}'/><target dev='sdb' bus='sata'/><readonly/></disk>\
<disk type='file' device='cdrom'><driver name='qemu' type='raw'/><source file='{GUEST_TOOLS_MEDIA}'/><target dev='sdc' bus='sata'/><readonly/></disk>\
<disk type='file' device='cdrom'><driver name='qemu' type='raw'/><source file='{VIRTIO_DRIVERS_MEDIA}'/><target dev='sdd' bus='sata'/><readonly/></disk>\
<interface type='network'><mac address='52:54:00:71:4f:53'/><source network='{network}'/><model type='virtio'/></interface>\
<channel type='unix'><target type='virtio' name='org.qemu.guest_agent.0'/></channel>\
<channel type='spicevmc'><target type='virtio' name='com.redhat.spice.0'/></channel>\
{vsock}\
<tpm model='{tpm}'><backend type='emulator' version='2.0'/></tpm>\
<rng model='virtio'><backend model='random'>/dev/urandom</backend></rng>\
<!-- Windows virtio-gpu is a paravirtual 2D fallback; do not infer a Linux virgl 3D path. -->\
<video><model type='virtio' heads='1' primary='yes'><acceleration accel3d='no'/></model></video>\
<graphics type='spice' autoport='yes'><listen type='none'/><clipboard copypaste='no'/><filetransfer enable='no'/></graphics>\
<sound model='ich9'><codec type='micro'/><audio id='1'/></sound>\
<audio id='1' type='spice'/>\
<memballoon model='virtio'/></devices></domain>",
        config.domain_name,
        config.domain_uuid,
        config.domain_uuid,
        config.domain_uuid,
        config.performance.memory_mib,
        config.performance.memory_mib,
        config.performance.vcpus,
        config.performance.io_threads,
    ))
}

fn xml_path(path: &Path) -> Result<String, VmBackendError> {
    let text = path.to_str().ok_or(VmBackendError::NonUtf8Path)?;
    Ok(quick_xml::escape::escape(text).into_owned())
}

#[derive(Debug, Error)]
pub enum VmBackendError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("virsh failed with code {code:?}: {stderr}")]
    Virsh { code: Option<i32>, stderr: String },
    #[error("libvirt returned non-UTF-8 output")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
    #[error("an existing domain named tilo-windows-appliance is not the configured Tilo appliance")]
    ForeignDomainCollision,
    #[error("the appliance domain disappeared while it was being started")]
    DomainDisappeared,
    #[error("the appliance is paused or in an unknown state; use the administrator recovery flow")]
    UnsafeDomainState,
    #[error("libvirt lifecycle action is not supported by this backend: {0}")]
    UnsupportedAction(&'static str),
    #[error("egress lease duration must be between 1 and 1800 seconds")]
    InvalidEgressTtl,
    #[error("egress lease identifier is invalid")]
    InvalidEgressLease,
    #[error("graphics partitions can be changed only while the Windows appliance is shut off")]
    GraphicsRequiresShutoff,
    #[error("graphics assignment is not a canonical SR-IOV VF or mediated-device identifier")]
    InvalidGraphicsAssignment,
    #[error("libvirt paths must be valid UTF-8")]
    NonUtf8Path,
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    struct Runner {
        replies: Mutex<VecDeque<CommandOutput>>,
        requests: Mutex<Vec<CommandRequest>>,
    }

    impl Runner {
        fn new(replies: Vec<CommandOutput>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for Runner {
        fn run(&self, request: &CommandRequest) -> Result<CommandOutput, CommandError> {
            self.requests.lock().unwrap().push(request.clone());
            Ok(self.replies.lock().unwrap().pop_front().unwrap())
        }
    }

    fn config() -> ApplianceConfig {
        ApplianceConfig::recommended_for(
            crate::GuestArchitecture::X86_64,
            "01234567-89ab-cdef-0123-456789abcdef",
            "/var/lib/tilo/windows/a&b.qcow2",
            "/var/lib/tilo/windows/nvram.fd",
        )
    }

    #[test]
    fn generated_domains_are_headless_default_deny_and_architecture_correct() {
        // Phase `domain_is_headless_and_default_deny`.
        {
            let xml = render_domain_xml(&config()).unwrap();
            assert!(xml.contains("type='kvm'"));
            assert!(xml.contains("host-passthrough"));
            assert!(xml.contains("<hyperv mode='custom'>"));
            assert!(xml.contains("<tpm model='tpm-crb'>"));
            assert!(
                xml.contains("<smbios mode='sysinfo'/>")
                    && xml.contains(
                        "<entry name='serial'>01234567-89ab-cdef-0123-456789abcdef</entry>"
                    )
            );
            assert!(xml.contains("<serial>tilo-windows-system</serial>"));
            assert!(xml.contains("<listen type='none'/>"));
            assert!(xml.contains("<sound model='ich9'>"));
            assert!(xml.contains("name='com.redhat.spice.0'"));
            assert!(xml.contains("<controller type='virtio-serial' index='0'/>"));
            assert!(xml.contains("<acceleration accel3d='no'/>"));
            assert!(xml.contains("<clipboard copypaste='no'/>"));
            assert!(xml.contains("<filetransfer enable='no'/>"));
            assert!(xml.contains("<audio id='1' type='spice'/>"));
            assert!(xml.contains("a&amp;b.qcow2"));
            assert!(xml.contains(INSTALL_MEDIA));
            assert!(xml.contains(GUEST_TOOLS_MEDIA));
            assert!(xml.contains("<boot dev='hd'/><boot dev='cdrom'/>"));
            assert!(!xml.contains("<filesystem"));
            assert!(!xml.contains("type='bridge'"));
            assert!(!xml.contains("listen='0.0.0.0'"));
            assert!(!xml.contains("<redirdev"));
        }
        // Phase `cpu_allocation_uses_one_socket_for_both_architectures_and_modes`.
        {
            for architecture in [
                crate::GuestArchitecture::X86_64,
                crate::GuestArchitecture::Aarch64,
            ] {
                for mode in [CpuMode::HostPassthrough, CpuMode::HostModel] {
                    for cores in [1, 4, 64] {
                        let mut configuration = ApplianceConfig::recommended_for(
                            architecture,
                            "01234567-89ab-cdef-0123-456789abcdef",
                            "/var/lib/tilo/windows/system.qcow2",
                            "/var/lib/tilo/windows/firmware-state",
                        );
                        configuration.performance.cpu_mode = mode;
                        configuration.performance.vcpus = cores;
                        let xml = render_domain_xml(&configuration).unwrap();
                        assert!(xml.contains(&format!("<vcpu placement='static'>{cores}</vcpu>")));
                        assert!(xml.contains(&format!(
                            "<topology sockets='1' cores='{cores}' threads='1'/>"
                        )));
                    }
                }
            }
        }
        // Phase `arm64_domain_uses_protected_variables_and_native_devices`.
        {
            let configuration = ApplianceConfig::recommended_for(
                crate::GuestArchitecture::Aarch64,
                "01234567-89ab-cdef-0123-456789abcdef",
                "/var/lib/tilo/windows/system.qcow2",
                "/var/lib/tilo/windows/vars.json",
            );
            let xml = render_domain_xml(&configuration).unwrap();
            assert!(xml.contains("arch='aarch64' machine='virt'"));
            assert!(xml.contains("/usr/bin/qemu-system-aarch64"));
            assert!(xml.contains("<varstore path='/var/lib/tilo/windows/vars.json'/>"));
            assert!(xml.contains("enabled='yes' name='secure-boot'"));
            assert!(xml.contains("enabled='yes' name='enrolled-keys'"));
            assert!(xml.contains("<gic version='3'/>"));
            assert!(!xml.contains("<pm>"));
            assert!(xml.contains("<tpm model='tpm-tis'>"));
            for x86_only in [
                "<smm",
                "<apic",
                "<hyperv",
                "<nvram",
                "name='pit'",
                "name='hpet'",
            ] {
                assert!(!xml.contains(x86_only), "unexpected x86 device: {x86_only}");
            }
            assert!(xml.contains("<listen type='none'/>"));
            assert!(xml.contains("<clipboard copypaste='no'/>"));
            assert!(xml.contains("<filetransfer enable='no'/>"));
            assert!(!xml.contains("<filesystem"));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn domain_ownership_definition_and_devices_change_only_safely() {
        // Phase `foreign_same_named_domain_is_refused`.
        {
            let runner = Runner::new(vec![
                CommandOutput::successful("tilo-windows-appliance\n"),
                CommandOutput::successful(
                    "<domain><name>tilo-windows-appliance</name><uuid>ffffffff-ffff-ffff-ffff-ffffffffffff</uuid></domain>",
                ),
            ]);
            let backend = LibvirtBackend::new(runner);
            assert!(matches!(
                backend.ensure_singleton(&config()),
                Err(VmBackendError::ForeignDomainCollision)
            ));
        }
        // Phase `an_owned_domain_survives_a_metadata_namespace_migration`.
        {
            let domain = render_domain_xml(&config())
                .unwrap()
                .replace("https://tilo.sh/vm/1", "https://legacy.example.test/vm/1");
            let runner = Runner::new(vec![
                CommandOutput::successful("tilo-windows-appliance\n"),
                CommandOutput::successful(domain),
            ]);
            let backend = LibvirtBackend::new(runner);
            backend.ensure_singleton(&config()).unwrap();
        }
        // Phase `absent_domain_is_defined_through_stdin`.
        {
            let runner = Runner::new(vec![
                CommandOutput::successful(""),
                CommandOutput::successful("Domain defined"),
            ]);
            let backend = LibvirtBackend::new(runner);
            backend.ensure_singleton(&config()).unwrap();
            let requests = backend.runner.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests[1].args.iter().any(|arg| arg == "/dev/stdin"));
            assert!(String::from_utf8_lossy(&requests[1].stdin).contains("<domain type='kvm'>"));
            drop(requests);
        }
        // Phase `an_explicit_partition_is_attached_persistently_only_while_shut_off`.
        {
            let domain = render_domain_xml(&config()).unwrap();
            let runner = Runner::new(vec![
                CommandOutput::successful("tilo-windows-appliance\n"),
                CommandOutput::successful(domain.as_str()),
                CommandOutput::successful("tilo-windows-appliance\n"),
                CommandOutput::successful("shut off\n"),
                CommandOutput::successful("Device attached\n"),
            ]);
            let backend = LibvirtBackend::new(runner);
            backend
                .attach_graphics(
                    &config(),
                    &GraphicsAssignment::SriovVf {
                        pci_address: "0000:03:00.1".to_string(),
                    },
                )
                .unwrap();
            let attach = backend
                .runner
                .requests
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .clone();
            assert!(attach.args.iter().any(|arg| arg == "attach-device"));
            assert!(attach.args.iter().any(|arg| arg == "--config"));
            assert!(!attach.args.iter().any(|arg| arg == "--live"));
            assert!(String::from_utf8_lossy(&attach.stdin).contains("managed='no'"));
        }
        // Phase `graphics_attachment_refuses_a_running_guest`.
        {
            let domain = render_domain_xml(&config()).unwrap();
            let runner = Runner::new(vec![
                CommandOutput::successful("tilo-windows-appliance\n"),
                CommandOutput::successful(domain.as_str()),
                CommandOutput::successful("tilo-windows-appliance\n"),
                CommandOutput::successful("running\n"),
            ]);
            let backend = LibvirtBackend::new(runner);
            assert!(matches!(
                backend.attach_graphics(
                    &config(),
                    &GraphicsAssignment::MediatedDevice {
                        uuid: "53764d0e-85a0-42b4-af5c-2046b460b1dc".to_string(),
                    },
                ),
                Err(VmBackendError::GraphicsRequiresShutoff)
            ));
        }
        // Phase `enrollment_media_is_detached_live_and_persistently`.
        {
            let runner = Runner::new(vec![
                CommandOutput::successful("<disk><target dev='sdb'/></disk>"),
                CommandOutput::successful("Disk detached"),
                CommandOutput::successful("<disk><target dev=\"sdc\"/></disk>"),
                CommandOutput::successful("Disk detached"),
                CommandOutput::successful("<disk><target dev='sdd'/></disk>"),
                CommandOutput::successful("Disk detached"),
            ]);
            let backend = LibvirtBackend::new(runner);
            backend.retire_installation_media(&config()).unwrap();
            let requests = backend.runner.requests.lock().unwrap();
            assert_eq!(requests.len(), 6);
            for index in [1, 3, 5] {
                assert!(requests[index].args.iter().any(|arg| arg == "--live"));
                assert!(requests[index].args.iter().any(|arg| arg == "--config"));
            }
            drop(requests);
        }
        // Phase `already_retired_media_is_an_idempotent_success`.
        {
            let runner = Runner::new(vec![
                CommandOutput::successful("<domain/>"),
                CommandOutput::successful("<domain/>"),
                CommandOutput::successful("<domain/>"),
            ]);
            let backend = LibvirtBackend::new(runner);
            backend.retire_installation_media(&config()).unwrap();
            assert_eq!(backend.runner.requests.lock().unwrap().len(), 3);
        }
    }
}
