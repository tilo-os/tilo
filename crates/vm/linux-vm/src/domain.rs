//! The libvirt domain of a Linux VM: defined in the user's own `qemu:///session`,
//! with no display. Shares are attached live and are never part of the definition,
//! so a snapshot never contains one and a revoked grant is gone at the next sync.

use std::path::Path;

use libvirt_domain::{metadata_xml, vsock_device_xml, xml_path, xml_text};
use thiserror::Error;

use crate::config::VmConfig;
use crate::grants::HostShare;
use crate::transport::{QEMU_NAMESPACE, VsockBackend, hybrid_commandline_xml};

/// The kind in the metadata marker that makes a domain Tilo's Linux VM.
const METADATA_KIND: &str = "linux-vm";

/// Empty PCI Express root ports for live-attached shares.
const HOTPLUG_PORTS: usize = 8;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DomainError {
    #[error("a path in the VM configuration is not UTF-8")]
    NonUtf8Path,
}

pub fn render_domain_xml(config: &VmConfig, backend: VsockBackend) -> Result<String, DomainError> {
    let disk = xml_path(&config.paths.disk()).ok_or(DomainError::NonUtf8Path)?;
    let console = xml_path(&config.paths.console_log()).ok_or(DomainError::NonUtf8Path)?;
    let name = xml_text(&config.domain_name);
    let metadata = metadata_xml(METADATA_KIND, &[("guest", config.guest.as_str())]);
    let (namespace, vsock, commandline) = match backend {
        VsockBackend::Kernel => (String::new(), vsock_device_xml(), String::new()),
        VsockBackend::Hybrid => (
            format!(" xmlns:qemu='{QEMU_NAMESPACE}'"),
            "",
            hybrid_commandline_xml(&config.paths.vsock_vhost_user()),
        ),
    };
    let memory = &config.memory;
    // q35 hot-plugs only into empty root ports, so shares need spares; naming any PCI controller
    // makes libvirt want the root complex named first.
    let hotplug_ports = format!(
        "<controller type='pci' index='0' model='pcie-root'/>{}",
        "<controller type='pci' model='pcie-root-port'/>".repeat(HOTPLUG_PORTS)
    );
    Ok(format!(
        "<domain type='kvm'{namespace}>\
<name>{name}</name>\
{metadata}\
<memory unit='MiB'>{max}</memory><currentMemory unit='MiB'>{base}</currentMemory>\
<memoryBacking><source type='memfd'/><access mode='shared'/></memoryBacking>\
<vcpu placement='static'>{vcpus}</vcpu>\
<cpu mode='host-passthrough' check='none' migratable='on'/>\
<os firmware='efi'><type arch='x86_64' machine='q35'>hvm</type><boot dev='hd'/></os>\
<features><acpi/><apic/></features>\
<clock offset='utc'/>\
<on_poweroff>destroy</on_poweroff><on_reboot>restart</on_reboot><on_crash>destroy</on_crash>\
<pm><suspend-to-mem enabled='no'/><suspend-to-disk enabled='no'/></pm>\
<devices>\
{hotplug_ports}\
<disk type='file' device='disk'><driver name='qemu' type='qcow2' discard='unmap'/>\
<source file='{disk}'/><target dev='vda' bus='virtio'/></disk>\
<interface type='user'><backend type='passt'/><model type='virtio'/></interface>\
{vsock}\
<memballoon model='virtio'><stats period='5'/></memballoon>\
<rng model='virtio'><backend model='random'>/dev/urandom</backend></rng>\
<serial type='file'><source path='{console}'/><target port='0'/></serial>\
<video><model type='none'/></video>\
</devices>{commandline}</domain>",
        max = memory.max_mib,
        base = memory.base_mib,
        vcpus = config.vcpus,
    ))
}

/// One live-attached virtio-fs share served by an external `virtiofsd`.
pub fn filesystem_xml(share: &HostShare, socket: &Path) -> Result<String, DomainError> {
    let socket = xml_path(socket).ok_or(DomainError::NonUtf8Path)?;
    Ok(format!(
        "<filesystem type='mount'><driver type='virtiofs' queue='1024'/>\
<source socket='{socket}'/><target dir='{}'/></filesystem>",
        xml_text(&share.tag())
    ))
}

/// Whether a defined domain carries Tilo's marker for this guest.
#[must_use]
pub fn is_ours(domain_xml: &str, config: &VmConfig) -> bool {
    libvirt_domain::is_ours(
        domain_xml,
        METADATA_KIND,
        &[("guest", config.guest.as_str())],
    )
}

#[must_use]
pub fn domain_uuid(domain_xml: &str) -> Option<&str> {
    let uuid = domain_xml
        .split("<uuid>")
        .nth(1)?
        .split("</uuid>")
        .next()?
        .trim();
    (uuid.len() == 36 && uuid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')).then_some(uuid)
}

/// `domain_xml` naming `uuid`, placed right after the domain's name.
#[must_use]
pub fn with_uuid(domain_xml: &str, uuid: &str) -> String {
    domain_xml.replacen("</name>", &format!("</name><uuid>{uuid}</uuid>"), 1)
}

/// The tags of virtio-fs shares attached to a live domain.
#[must_use]
pub fn attached_tags(domain_xml: &str) -> Vec<String> {
    domain_xml
        .split("<filesystem")
        .skip(1)
        .filter_map(|device| {
            let target = device
                .split("</filesystem>")
                .next()?
                .split("<target dir=")
                .nth(1)?;
            let quote = target.chars().next()?;
            Some(target.strip_prefix(quote)?.split(quote).next()?.to_string())
        })
        .collect()
}
