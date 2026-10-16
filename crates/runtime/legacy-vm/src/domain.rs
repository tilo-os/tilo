//! The libvirt domain of a legacy VM.
//!
//! Defined in the user's own `qemu:///session`: QEMU runs as the user, the
//! network is user-mode (passt), and nothing needs a privileged daemon. The
//! domain has no display: windows leave through `waypipe` over vsock.
//! Memory is shared (memfd) because virtio-fs needs it; shares themselves are
//! never part of the definition but attached live, so a snapshot never
//! contains one and a revoked grant is gone at the next sync.

use std::path::Path;

use libvirt_domain::{vsock_device_xml, xml_path, xml_text};
use thiserror::Error;

use crate::config::VmConfig;
use crate::grants::HostShare;
use crate::transport::{QEMU_NAMESPACE, VsockBackend, hybrid_commandline_xml};

/// Marks a domain as Tilo's; a same-named domain without it is not touched.
pub const METADATA_ELEMENT: &str = "<tilo:legacy-vm";

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
    let guest = config.guest.as_str();
    let (namespace, vsock, commandline) = match backend {
        VsockBackend::Kernel => (String::new(), vsock_device_xml(None), String::new()),
        VsockBackend::Hybrid => (
            format!(" xmlns:qemu='{QEMU_NAMESPACE}'"),
            String::new(),
            hybrid_commandline_xml(&config.paths.vsock_vhost_user()),
        ),
    };
    let memory = &config.memory;
    // q35 hot-plugs only into empty root ports, and libvirt adds ports just
    // for the devices it knows at definition time: shares need spares.
    // Naming any PCI controller makes libvirt want the root complex named
    // first.
    let hotplug_ports = format!(
        "<controller type='pci' index='0' model='pcie-root'/>{}",
        "<controller type='pci' model='pcie-root-port'/>".repeat(HOTPLUG_PORTS)
    );
    Ok(format!(
        "<domain type='kvm'{namespace}>\
<name>{name}</name>\
<metadata><tilo:legacy-vm xmlns:tilo='https://tilo.sh/vm/1' guest='{guest}'/></metadata>\
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

/// Whether a defined domain carries Tilo's marker.
#[must_use]
pub fn is_ours(domain_xml: &str, config: &VmConfig) -> bool {
    // libvirt re-serialises metadata with double quotes.
    let guest = config.guest.as_str();
    domain_xml.contains(METADATA_ELEMENT)
        && (domain_xml.contains(&format!("guest='{guest}'"))
            || domain_xml.contains(&format!("guest=\"{guest}\"")))
}

/// The `<uuid>` of a defined domain.
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use legacy_agent_protocol::GuestOs;

    use super::*;
    use crate::config::Paths;

    fn config() -> VmConfig {
        VmConfig::new(
            GuestOs::Linux,
            "/c/base.qcow2".into(),
            Paths::under(
                Path::new("/d"),
                Path::new("/run/user/1000"),
                Path::new("/c"),
                GuestOs::Linux,
            ),
        )
    }

    #[test]
    fn the_domain_is_unprivileged_headless_and_shares_nothing_by_definition() {
        let config = config();
        let xml = render_domain_xml(&config, VsockBackend::Kernel).unwrap();
        // phase: the definition is session-safe and display-free
        {
            for required in [
                "<name>tilo-legacy-linux</name>",
                "<memory unit='MiB'>6144</memory><currentMemory unit='MiB'>1536</currentMemory>",
                "<source type='memfd'/><access mode='shared'/>",
                "<backend type='passt'/>",
                "<cid auto='yes'/>",
                "<memballoon model='virtio'>",
                "<source file='/d/tilo/legacy/linux/disk.qcow2'/>",
                "<video><model type='none'/></video>",
            ] {
                assert!(xml.contains(required), "missing {required}");
            }
            for forbidden in [
                "<filesystem",
                "type='network'",
                "<hostdev",
                "spice",
                "<graphics",
            ] {
                assert!(!xml.contains(forbidden), "unexpected {forbidden}");
            }
            assert!(is_ours(&xml, &config));
            let hybrid = render_domain_xml(&config, VsockBackend::Hybrid).unwrap();
            assert!(hybrid.starts_with("<domain type='kvm' xmlns:qemu="));
            assert!(hybrid.contains("vhost-user-vsock-pci"));
            assert!(!hybrid.contains("<vsock"));
            assert!(is_ours(&hybrid, &config));
            assert!(
                is_ours(
                    "<metadata><tilo:legacy-vm xmlns:tilo=\"https://tilo.sh/vm/1\" guest=\"linux\"/></metadata>",
                    &config
                ),
                "libvirt's own serialisation is still ours"
            );
            let uuid = "b9a49084-2e51-412a-ae9c-c1f9d51b0863";
            let live = format!("<domain><name>x</name>\n  <uuid>{uuid}</uuid></domain>");
            assert_eq!(domain_uuid(&live), Some(uuid));
            let redefined = with_uuid(&xml, uuid);
            assert!(redefined.contains(&format!("</name><uuid>{uuid}</uuid>")));
            assert_eq!(domain_uuid(&redefined), Some(uuid));
            assert_eq!(domain_uuid("<domain/>"), None);
            assert!(!is_ours(
                "<domain><name>tilo-legacy-linux</name></domain>",
                &config
            ));
        }
        // phase: a share attaches by socket under its tag, and is found again
        {
            let share = HostShare {
                path: PathBuf::from("/var/home/you/src"),
                read_only: false,
            };
            let device = filesystem_xml(&share, Path::new("/run/user/1000/x.sock")).unwrap();
            assert!(device.contains("<driver type='virtiofs' queue='1024'/>"));
            assert!(device.contains("<source socket='/run/user/1000/x.sock'/>"));
            let live = format!("<domain><devices>{device}<disk/></devices></domain>");
            assert_eq!(attached_tags(&live), [share.tag()]);
            assert!(attached_tags(&xml).is_empty());
        }
    }
}
