use std::ffi::OsString;
use std::path::Path;

use thiserror::Error;

use crate::{
    APPLIANCE_DOMAIN_NAME, ApplianceConfig, CommandError, CommandOutput, CommandRequest,
    CommandRunner, ConfigError, CpuMode,
};

const LIBVIRT_URI: &str = "qemu:///system";
const METADATA_MARKER: &str = "<qos:windows-appliance xmlns:qos='https://qos.dev/vm/1'/>";
const METADATA_ELEMENT: &str = "<qos:windows-appliance";
const METADATA_NAMESPACE: &str = "https://qos.dev/vm/1";
const INSTALL_MEDIA: &str = "/var/lib/qos/windows/install-media.iso";
const GUEST_TOOLS_MEDIA: &str = "/var/lib/qos/windows/qos-guest-tools.iso";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainState {
    Absent,
    ShutOff,
    Running,
    Paused,
    Other,
}

pub trait VmBackend: Send + Sync {
    /// Defines the singleton if absent and rejects a same-named foreign domain.
    fn ensure_singleton(&self, config: &ApplianceConfig) -> Result<(), VmBackendError>;
    fn state(&self, config: &ApplianceConfig) -> Result<DomainState, VmBackendError>;
    fn start(&self, config: &ApplianceConfig) -> Result<(), VmBackendError>;
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

    #[must_use]
    pub fn with_virsh(mut self, path: impl Into<OsString>) -> Self {
        self.virsh = path.into();
        self
    }
}

impl<R: CommandRunner> LibvirtBackend<R> {
    fn virsh<I, S>(&self, args: I) -> Result<CommandOutput, VmBackendError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let request = CommandRequest::new(self.virsh.clone())
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
            if !xml.contains(&expected_uuid)
                || !xml.contains(METADATA_ELEMENT)
                || !xml.contains(METADATA_NAMESPACE)
            {
                return Err(VmBackendError::ForeignDomainCollision);
            }
            return Ok(());
        }

        let xml = render_domain_xml(config)?;
        let request = CommandRequest::new(self.virsh.clone())
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
        if !names.lines().any(|name| name.trim() == config.domain_name) {
            return Ok(DomainState::Absent);
        }
        let state = self.successful(["domstate", APPLIANCE_DOMAIN_NAME])?;
        let state = String::from_utf8(state.stdout)?.trim().to_ascii_lowercase();
        Ok(match state.as_str() {
            "running" | "idle" | "in shutdown" => DomainState::Running,
            "shut off" | "crashed" => DomainState::ShutOff,
            "paused" | "pmsuspended" => DomainState::Paused,
            _ => DomainState::Other,
        })
    }

    fn start(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        self.ensure_singleton(config)?;
        match self.state(config)? {
            DomainState::Running => Ok(()),
            DomainState::ShutOff => self
                .successful(["start", APPLIANCE_DOMAIN_NAME])
                .map(|_| ()),
            DomainState::Absent => Err(VmBackendError::DomainDisappeared),
            DomainState::Paused | DomainState::Other => Err(VmBackendError::UnsafeDomainState),
        }
    }

    fn retire_installation_media(&self, config: &ApplianceConfig) -> Result<(), VmBackendError> {
        config.validate()?;
        for target in ["sdb", "sdc"] {
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
/// or clipboard device. The SPICE graphics device has no listener and exists
/// solely so an administrator can attach through libvirt for recovery.
pub fn render_domain_xml(config: &ApplianceConfig) -> Result<String, VmBackendError> {
    config.validate()?;
    let disk = xml_path(&config.system_disk)?;
    let loader = xml_path(&config.firmware.secure_boot_loader)?;
    let nvram_template = xml_path(&config.firmware.nvram_template)?;
    let nvram = xml_path(&config.firmware.nvram)?;
    let network = xml_escape(&config.host_only_network);
    let cpu = match config.performance.cpu_mode {
        CpuMode::HostPassthrough => "<cpu mode='host-passthrough' check='none' migratable='off'/>",
        CpuMode::HostModel => "<cpu mode='host-model' check='partial'/>",
    };
    Ok(format!(
        "<domain type='kvm'>\
<name>{}</name><uuid>{}</uuid>\
<metadata>{METADATA_MARKER}</metadata>\
<memory unit='MiB'>{}</memory><currentMemory unit='MiB'>{}</currentMemory>\
<vcpu placement='static'>{}</vcpu><iothreads>{}</iothreads>{cpu}\
<features><acpi/><apic/><hyperv mode='custom'>\
<relaxed state='on'/><vapic state='on'/><spinlocks state='on' retries='8191'/>\
<vpindex state='on'/><runtime state='on'/><synic state='on'/><stimer state='on'>\
<direct state='on'/></stimer><frequencies state='on'/><tlbflush state='on'/>\
<ipi state='on'/><evmcs state='on'/></hyperv><vmport state='off'/><smm state='on'/></features>\
<clock offset='localtime'><timer name='rtc' tickpolicy='catchup'/>\
<timer name='pit' tickpolicy='delay'/><timer name='hpet' present='no'/>\
<timer name='hypervclock' present='yes'/></clock>\
<os firmware='efi'><type arch='x86_64' machine='q35'>hvm</type>\
<loader readonly='yes' secure='yes' type='pflash'>{loader}</loader>\
<nvram template='{nvram_template}'>{nvram}</nvram><boot dev='hd'/><boot dev='cdrom'/></os>\
<pm><suspend-to-mem enabled='no'/><suspend-to-disk enabled='no'/></pm>\
<devices><emulator>/usr/bin/qemu-system-x86_64</emulator>\
<controller type='scsi' index='0' model='virtio-scsi'><driver iothread='1'/></controller>\
<disk type='file' device='disk'><driver name='qemu' type='qcow2' cache='none' io='native' discard='unmap' detect_zeroes='unmap'/>\
<source file='{disk}'/><target dev='sda' bus='scsi'/></disk>\
<disk type='file' device='cdrom'><driver name='qemu' type='raw'/><source file='{INSTALL_MEDIA}'/><target dev='sdb' bus='scsi'/><readonly/></disk>\
<disk type='file' device='cdrom'><driver name='qemu' type='raw'/><source file='{GUEST_TOOLS_MEDIA}'/><target dev='sdc' bus='scsi'/><readonly/></disk>\
<interface type='network'><mac address='52:54:00:71:4f:53'/><source network='{network}'/><model type='virtio'/></interface>\
<channel type='unix'><target type='virtio' name='org.qemu.guest_agent.0'/></channel>\
<vsock model='virtio'><cid auto='no' address='{}'/></vsock>\
<tpm model='tpm-crb'><backend type='emulator' version='2.0'/></tpm>\
<rng model='virtio'><backend model='random'>/dev/urandom</backend></rng>\
<video><model type='virtio' heads='1' primary='yes'/></video>\
<graphics type='spice' autoport='yes'><listen type='none'/></graphics>\
<memballoon model='virtio'/></devices></domain>",
        config.domain_name,
        config.domain_uuid,
        config.performance.memory_mib,
        config.performance.memory_mib,
        config.performance.vcpus,
        config.performance.io_threads,
        config.guest_cid,
    ))
}

fn xml_path(path: &Path) -> Result<String, VmBackendError> {
    let text = path.to_str().ok_or(VmBackendError::NonUtf8Path)?;
    Ok(xml_escape(text))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
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
    #[error("an existing domain named qos-windows-appliance is not the configured qOS appliance")]
    ForeignDomainCollision,
    #[error("the appliance domain disappeared while it was being started")]
    DomainDisappeared,
    #[error("the appliance is paused or in an unknown state; use the administrator recovery flow")]
    UnsafeDomainState,
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
        ApplianceConfig::recommended(
            "01234567-89ab-cdef-0123-456789abcdef",
            "/var/lib/qos/windows/a&b.qcow2",
            "/var/lib/qos/windows/nvram.fd",
        )
    }

    #[test]
    fn domain_is_headless_and_default_deny() {
        let xml = render_domain_xml(&config()).unwrap();
        assert!(xml.contains("type='kvm'"));
        assert!(xml.contains("host-passthrough"));
        assert!(xml.contains("<hyperv mode='custom'>"));
        assert!(xml.contains("<tpm model='tpm-crb'>"));
        assert!(xml.contains("<listen type='none'/>"));
        assert!(xml.contains("a&amp;b.qcow2"));
        assert!(xml.contains(INSTALL_MEDIA));
        assert!(xml.contains(GUEST_TOOLS_MEDIA));
        assert!(xml.contains("<boot dev='hd'/><boot dev='cdrom'/>"));
        assert!(!xml.contains("<filesystem"));
        assert!(!xml.contains("type='bridge'"));
        assert!(!xml.contains("listen='0.0.0.0'"));
        assert!(!xml.contains("<redirdev"));
    }

    #[test]
    fn foreign_same_named_domain_is_refused() {
        let runner = Runner::new(vec![
            CommandOutput::successful("qos-windows-appliance\n"),
            CommandOutput::successful(
                "<domain><name>qos-windows-appliance</name><uuid>ffffffff-ffff-ffff-ffff-ffffffffffff</uuid></domain>",
            ),
        ]);
        let backend = LibvirtBackend::new(runner);
        assert!(matches!(
            backend.ensure_singleton(&config()),
            Err(VmBackendError::ForeignDomainCollision)
        ));
    }

    #[test]
    fn absent_domain_is_defined_through_stdin() {
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

    #[test]
    fn enrollment_media_is_detached_live_and_persistently() {
        let runner = Runner::new(vec![
            CommandOutput::successful("<disk><target dev='sdb'/></disk>"),
            CommandOutput::successful("Disk detached"),
            CommandOutput::successful("<disk><target dev=\"sdc\"/></disk>"),
            CommandOutput::successful("Disk detached"),
        ]);
        let backend = LibvirtBackend::new(runner);
        backend.retire_installation_media(&config()).unwrap();
        let requests = backend.runner.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        for index in [1, 3] {
            assert!(requests[index].args.iter().any(|arg| arg == "--live"));
            assert!(requests[index].args.iter().any(|arg| arg == "--config"));
        }
        drop(requests);
    }

    #[test]
    fn already_retired_media_is_an_idempotent_success() {
        let runner = Runner::new(vec![
            CommandOutput::successful("<domain/>"),
            CommandOutput::successful("<domain/>"),
        ]);
        let backend = LibvirtBackend::new(runner);
        backend.retire_installation_media(&config()).unwrap();
        assert_eq!(backend.runner.requests.lock().unwrap().len(), 2);
    }
}
