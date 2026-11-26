use std::path::Path;

pub use libvirt_domain::DomainState;
use libvirt_domain::{BoundedVirsh, Connection, Virsh, VirshError, VirshRunner};
use thiserror::Error;

use crate::{
    APPLIANCE_DOMAIN_NAME, ApplianceConfig, ConfigError, CpuMode, FirmwareConfig,
    GraphicsAssignment,
};

const METADATA_KIND: &str = "windows-appliance";
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
pub struct LibvirtBackend<R = BoundedVirsh> {
    virsh: Virsh<R>,
}

impl LibvirtBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::with_runner(BoundedVirsh)
    }
}

impl Default for LibvirtBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> LibvirtBackend<R> {
    #[must_use]
    pub fn with_runner(runner: R) -> Self {
        Self {
            virsh: Virsh::with_runner(runner, Connection::System, APPLIANCE_DOMAIN_NAME),
        }
    }
}

impl<R: VirshRunner> LibvirtBackend<R> {
    /// Persistently attach one already-prepared GPU partition. The VM must be
    /// off; the caller has separately proved this exact VF/mdev through the
    /// read-only capability probe and an owner-authorised boundary.
    pub fn attach_graphics(
        &self,
        config: &ApplianceConfig,
        assignment: &GraphicsAssignment,
    ) -> Result<(), VmBackendError> {
        let xml = self.graphics_change(config, assignment)?;
        Ok(self.virsh.attach_device(&xml, &["--config"])?)
    }

    /// Detach only the exact partition recorded by Tilo at attachment time.
    pub fn detach_graphics(
        &self,
        config: &ApplianceConfig,
        assignment: &GraphicsAssignment,
    ) -> Result<(), VmBackendError> {
        let xml = self.graphics_change(config, assignment)?;
        Ok(self.virsh.detach_device(&xml, &["--config"])?)
    }

    fn graphics_change(
        &self,
        config: &ApplianceConfig,
        assignment: &GraphicsAssignment,
    ) -> Result<String, VmBackendError> {
        self.ensure_singleton(config)?;
        if self.state(config)? != DomainState::ShutOff {
            return Err(VmBackendError::GraphicsRequiresShutoff);
        }
        assignment
            .domain_xml()
            .ok_or(VmBackendError::InvalidGraphicsAssignment)
    }
}

impl<R: VirshRunner> VmBackend for LibvirtBackend<R> {
    fn ensure_singleton(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        config.validate()?;
        if self.virsh.exists()? {
            let xml = self.virsh.dumpxml()?;
            let expected_uuid = format!("<uuid>{}</uuid>", config.domain_uuid);
            let expected_smbios_serial =
                format!("<entry name='serial'>{}</entry>", config.domain_uuid);
            let expected_smbios_uuid = format!("<entry name='uuid'>{}</entry>", config.domain_uuid);
            if !xml.contains(&expected_uuid)
                || !xml.contains(&expected_smbios_serial)
                || !xml.contains(&expected_smbios_uuid)
                || !xml.contains("<serial>tilo-windows-system</serial>")
                || !xml.contains("<mac address='52:54:00:71:4f:53'/>")
                || !libvirt_domain::is_ours(&xml, METADATA_KIND, &[])
            {
                return Err(VmBackendError::ForeignDomainCollision);
            }
            return Ok(());
        }
        Ok(self.virsh.define(&render_domain_xml(config)?)?)
    }

    fn state(&self, config: &ApplianceConfig) -> Result<DomainState, VmBackendError> {
        config.validate()?;
        Ok(self.virsh.state()?)
    }

    fn start(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        match self.state(config)? {
            DomainState::Running => Ok(()),
            DomainState::ShutOff | DomainState::Crashed => Ok(self.virsh.start()?),
            DomainState::Absent => Err(VmBackendError::DomainDisappeared),
            DomainState::Paused
            | DomainState::Suspended
            | DomainState::Starting
            | DomainState::ShuttingDown
            | DomainState::Other => Err(VmBackendError::UnsafeDomainState),
        }
    }

    fn shutdown(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        match self.state(config)? {
            DomainState::Absent
            | DomainState::ShutOff
            | DomainState::ShuttingDown
            | DomainState::Crashed => Ok(()),
            DomainState::Running => Ok(self.virsh.shutdown()?),
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
            | DomainState::Suspended => Ok(self.virsh.destroy()?),
            DomainState::Other => Err(VmBackendError::UnsafeDomainState),
        }
    }

    fn restart(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        match self.state(config)? {
            DomainState::ShutOff | DomainState::Absent | DomainState::Crashed => self.start(config),
            DomainState::Running => Ok(self.virsh.on_domain("reboot", &["--mode", "acpi"])?),
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
            DomainState::Running => Ok(self.virsh.on_domain("suspend", &[])?),
            _ => Err(VmBackendError::UnsafeDomainState),
        }
    }

    fn resume(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        match self.state(config)? {
            DomainState::Running => Ok(()),
            DomainState::Paused | DomainState::Suspended => {
                Ok(self.virsh.on_domain("resume", &[])?)
            }
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
        self.virsh.on_domain(
            "attach-interface",
            &[
                "network",
                EGRESS_NETWORK,
                "--model",
                "virtio",
                "--mac",
                EGRESS_MAC,
                "--live",
            ],
        )?;
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
        match self.virsh.on_domain(
            "detach-interface",
            &["network", "--mac", EGRESS_MAC, "--live"],
        ) {
            Err(VirshError::Failed { stderr, .. })
                if ["not found", "no matching", "detached"]
                    .iter()
                    .any(|absent| stderr.to_ascii_lowercase().contains(absent)) =>
            {
                Ok(())
            }
            result => Ok(result?),
        }
    }

    fn retire_installation_media(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        config.validate()?;
        for target in ["sdb", "sdc", "sdd"] {
            let xml = self.virsh.dumpxml()?;
            let single_quoted = format!("dev='{target}'");
            let double_quoted = format!("dev=\"{target}\"");
            if !xml.contains(&single_quoted) && !xml.contains(&double_quoted) {
                continue;
            }
            self.virsh
                .on_domain("detach-disk", &[target, "--live", "--config"])?;
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
    let network = libvirt_domain::xml_text(&config.host_only_network);
    let cpu_attributes = match config.performance.cpu_mode {
        CpuMode::HostPassthrough => "mode='host-passthrough' check='none' migratable='off'",
        CpuMode::HostModel => "mode='host-model' check='partial'",
    };
    let cpu = format!(
        "<cpu {cpu_attributes}><topology sockets='1' cores='{}' threads='1'/></cpu>",
        config.performance.vcpus,
    );
    Ok(format!(
        "<domain type='kvm'>\
<name>{}</name><uuid>{}</uuid>\
{}\
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
        libvirt_domain::metadata_xml(METADATA_KIND, &[]),
        config.domain_uuid,
        config.domain_uuid,
        config.performance.memory_mib,
        config.performance.memory_mib,
        config.performance.vcpus,
        config.performance.io_threads,
    ))
}

fn xml_path(path: &Path) -> Result<String, VmBackendError> {
    libvirt_domain::xml_path(path).ok_or(VmBackendError::NonUtf8Path)
}

#[derive(Debug, Error)]
pub enum VmBackendError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Virsh(#[from] VirshError),
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
