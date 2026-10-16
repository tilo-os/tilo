//! Read-only discovery of Windows graphics acceleration candidates.
//!
//! The probe deliberately never creates SR-IOV virtual functions, creates an
//! mdev, unbinds a host driver, or assigns a device to the appliance.  Those
//! operations can take the active compositor GPU away from Tilo and need a
//! separately reviewed, owner-authorised lifecycle.  This module reports the
//! kernel's existing sysfs capabilities so that lifecycle can make an honest
//! decision instead of guessing from a PCI vendor or model name.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphicsRecommendation {
    /// A VF already bound to vfio-pci or an already-created mediated device is
    /// present.  It is a candidate for a later explicit assignment review.
    PartitionReadyForExplicitAssignment,
    /// The kernel driver exposes a partitioning interface, but no assignable
    /// partition has been prepared.
    PartitionCapableNotConfigured,
    /// No safe partition interface is visible. Keep the Tilo GPU on the host.
    Paravirtual2d,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GraphicsCapabilities {
    pub schema_version: u8,
    pub recommendation: GraphicsRecommendation,
    pub automatic_assignment: bool,
    pub devices: Vec<GraphicsDeviceCapability>,
    pub fallback: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GraphicsDeviceCapability {
    pub pci_address: String,
    pub vendor_id: Option<String>,
    pub device_id: Option<String>,
    pub host_driver: Option<String>,
    pub boot_vga: bool,
    pub iommu_group: Option<String>,
    pub sriov: Option<SriovCapability>,
    pub mediated_types: Vec<MediatedTypeCapability>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SriovCapability {
    pub total_vfs: u32,
    pub configured_vfs: u32,
    /// Configured display VFs which are the sole member of their IOMMU group,
    /// are not boot VGA, and are already bound to vfio-pci. The probe still
    /// does not claim they are assigned to Tilo.
    pub vfio_ready_vfs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MediatedTypeCapability {
    pub type_id: String,
    pub name: Option<String>,
    pub device_api: String,
    pub available_instances: u32,
    pub created_devices: Vec<String>,
}

/// One already-created hardware partition selected through the privileged
/// owner workflow. The probe never creates either kind.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GraphicsAssignment {
    SriovVf { pci_address: String },
    MediatedDevice { uuid: String },
}

#[derive(Debug, Error)]
pub enum GraphicsProbeError {
    #[error("cannot inspect PCI devices at {path}: {source}")]
    ReadPciDevices {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl GraphicsCapabilities {
    /// Probe Linux's documented PCI SR-IOV and VFIO mdev sysfs interfaces.
    pub fn probe() -> Result<Self, GraphicsProbeError> {
        Self::probe_sysfs(Path::new("/sys"))
    }

    fn probe_sysfs(sysfs: &Path) -> Result<Self, GraphicsProbeError> {
        let pci_root = sysfs.join("bus/pci/devices");
        let entries =
            fs::read_dir(&pci_root).map_err(|source| GraphicsProbeError::ReadPciDevices {
                path: pci_root.clone(),
                source,
            })?;
        let mut devices = BTreeMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(address) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            // PCI class 0x03 is a display controller. Do not treat unrelated
            // NIC/crypto SR-IOV support as a Windows graphics candidate.
            let Some(class) = read_trimmed(path.join("class")) else {
                continue;
            };
            if !class.to_ascii_lowercase().starts_with("0x03") {
                continue;
            }
            devices.insert(address.clone(), probe_device(address, &path));
        }
        let devices = devices.into_values().collect::<Vec<_>>();
        let ready = devices.iter().any(|device| {
            device
                .sriov
                .as_ref()
                .is_some_and(|sriov| !sriov.vfio_ready_vfs.is_empty())
                || device
                    .mediated_types
                    .iter()
                    .any(|kind| !kind.created_devices.is_empty())
        });
        let capable = devices.iter().any(|device| {
            device
                .sriov
                .as_ref()
                .is_some_and(|sriov| sriov.total_vfs > 0)
                || device
                    .mediated_types
                    .iter()
                    .any(|kind| kind.available_instances > 0 || !kind.created_devices.is_empty())
        });
        Ok(Self {
            schema_version: 1,
            recommendation: if ready {
                GraphicsRecommendation::PartitionReadyForExplicitAssignment
            } else if capable {
                GraphicsRecommendation::PartitionCapableNotConfigured
            } else {
                GraphicsRecommendation::Paravirtual2d
            },
            // Detection must never be confused with permission to detach or
            // mutate a device. Hardware attachment remains a separate design.
            automatic_assignment: false,
            devices,
            fallback: "virtio-gpu 2D + SPICE; RDP graphics for native RAIL",
        })
    }

    /// Accept only an existing display VF which is already IOMMU isolated and
    /// bound to vfio-pci. This does not detach the PF or its host driver.
    #[must_use]
    pub fn sriov_assignment(&self, pci_address: &str) -> Option<GraphicsAssignment> {
        if !valid_pci_address(pci_address)
            || !self.devices.iter().any(|device| {
                device
                    .sriov
                    .as_ref()
                    .is_some_and(|sriov| sriov.vfio_ready_vfs.iter().any(|vf| vf == pci_address))
            })
        {
            return None;
        }
        Some(GraphicsAssignment::SriovVf {
            pci_address: pci_address.to_string(),
        })
    }

    /// Accept only an mdev which the vendor driver has already created under
    /// a display controller's vfio-pci mediated type.
    #[must_use]
    pub fn mediated_assignment(&self, uuid: &str) -> Option<GraphicsAssignment> {
        if !crate::config::is_uuid(uuid)
            || !self.devices.iter().any(|device| {
                device.mediated_types.iter().any(|kind| {
                    kind.device_api == "vfio-pci"
                        && kind.created_devices.iter().any(|device| device == uuid)
                })
            })
        {
            return None;
        }
        Some(GraphicsAssignment::MediatedDevice {
            uuid: uuid.to_ascii_lowercase(),
        })
    }
}

impl GraphicsAssignment {
    #[must_use]
    pub fn domain_xml(&self) -> Option<String> {
        match self {
            Self::SriovVf { pci_address } => {
                let (domain, bus, slot, function) = parse_pci_address(pci_address)?;
                Some(format!(
                    "<hostdev mode='subsystem' type='pci' managed='no'>\
<driver name='vfio'/><source><address domain='0x{domain}' bus='0x{bus}' slot='0x{slot}' function='0x{function}'/></source></hostdev>"
                ))
            }
            Self::MediatedDevice { uuid } if crate::config::is_uuid(uuid) => Some(format!(
                "<hostdev mode='subsystem' type='mdev' managed='no' model='vfio-pci'>\
<source><address uuid='{}'/></source></hostdev>",
                uuid.to_ascii_lowercase()
            )),
            Self::MediatedDevice { .. } => None,
        }
    }
}

fn valid_pci_address(value: &str) -> bool {
    parse_pci_address(value).is_some()
}

fn parse_pci_address(value: &str) -> Option<(String, String, String, String)> {
    let (domain, rest) = value.split_once(':')?;
    let (bus, rest) = rest.split_once(':')?;
    let (slot, function) = rest.split_once('.')?;
    if domain.len() != 4
        || bus.len() != 2
        || slot.len() != 2
        || function.len() != 1
        || ![domain, bus, slot, function]
            .iter()
            .all(|field| field.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return None;
    }
    Some((
        domain.to_ascii_lowercase(),
        bus.to_ascii_lowercase(),
        slot.to_ascii_lowercase(),
        function.to_ascii_lowercase(),
    ))
}

fn probe_device(pci_address: String, path: &Path) -> GraphicsDeviceCapability {
    GraphicsDeviceCapability {
        pci_address,
        vendor_id: read_trimmed(path.join("vendor")),
        device_id: read_trimmed(path.join("device")),
        host_driver: symlink_name(path.join("driver")),
        boot_vga: read_trimmed(path.join("boot_vga")).as_deref() == Some("1"),
        iommu_group: symlink_name(path.join("iommu_group")),
        sriov: probe_sriov(path),
        mediated_types: probe_mediated_types(path),
    }
}

fn probe_sriov(path: &Path) -> Option<SriovCapability> {
    let total_vfs = read_u32(path.join("sriov_totalvfs"))?;
    let configured_vfs = read_u32(path.join("sriov_numvfs")).unwrap_or(0);
    let mut vfio_ready_vfs = Vec::new();
    let Ok(entries) = fs::read_dir(path) else {
        return Some(SriovCapability {
            total_vfs,
            configured_vfs,
            vfio_ready_vfs,
        });
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !name.strip_prefix("virtfn").is_some_and(|index| {
            !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            continue;
        }
        let Ok(vf) = fs::canonicalize(entry.path()) else {
            continue;
        };
        if !read_trimmed(vf.join("class"))
            .is_some_and(|class| class.to_ascii_lowercase().starts_with("0x03"))
            || read_trimmed(vf.join("boot_vga")).as_deref() == Some("1")
            || symlink_name(vf.join("driver")).as_deref() != Some("vfio-pci")
            || !has_exclusive_iommu_group(&vf)
        {
            continue;
        }
        if let Some(address) = vf.file_name().and_then(|name| name.to_str()) {
            vfio_ready_vfs.push(address.to_string());
        }
    }
    vfio_ready_vfs.sort();
    Some(SriovCapability {
        total_vfs,
        configured_vfs,
        vfio_ready_vfs,
    })
}

fn has_exclusive_iommu_group(device: &Path) -> bool {
    let Ok(group) = fs::canonicalize(device.join("iommu_group")) else {
        return false;
    };
    let Some(address) = device.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let members = directory_names(group.join("devices"));
    members.len() == 1 && members.first().is_some_and(|member| member == address)
}

fn probe_mediated_types(path: &Path) -> Vec<MediatedTypeCapability> {
    let root = path.join("mdev_supported_types");
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut types = Vec::new();
    for entry in entries.flatten() {
        let type_path = entry.path();
        let Some(type_id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(device_api) = read_trimmed(type_path.join("device_api")) else {
            continue;
        };
        // Tilo only reports mediated PCI display candidates usable through
        // VFIO; platform/AP mdev types cannot accelerate this Windows guest.
        if device_api != "vfio-pci" {
            continue;
        }
        let mut created_devices = directory_names(type_path.join("devices"));
        created_devices.sort();
        types.push(MediatedTypeCapability {
            type_id,
            name: read_trimmed(type_path.join("name")),
            device_api,
            available_instances: read_u32(type_path.join("available_instances")).unwrap_or(0),
            created_devices,
        });
    }
    types.sort_by(|left, right| left.type_id.cmp(&right.type_id));
    types
}

fn directory_names(path: PathBuf) -> Vec<String> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect()
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_u32(path: PathBuf) -> Option<u32> {
    read_trimmed(path)?.parse().ok()
}

fn symlink_name(path: PathBuf) -> Option<String> {
    fs::canonicalize(path)
        .ok()?
        .file_name()?
        .to_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::*;

    fn file(path: &Path, value: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn gpu_partitions_are_ready_only_when_isolated_and_otherwise_fall_back() {
        // Phase `no_partition_interface_keeps_the_host_gpu_and_reports_2d_fallback`.
        {
            let sysfs = TempDir::new().unwrap();
            let gpu = sysfs.path().join("bus/pci/devices/0000:00:02.0");
            file(&gpu.join("class"), "0x030000\n");
            file(&gpu.join("vendor"), "0x8086\n");
            file(&gpu.join("device"), "0x1234\n");
            let probe = GraphicsCapabilities::probe_sysfs(sysfs.path()).unwrap();
            assert_eq!(probe.recommendation, GraphicsRecommendation::Paravirtual2d);
            assert!(!probe.automatic_assignment);
            assert_eq!(probe.devices.len(), 1);
        }
        // Phase `mdev_capability_is_not_misreported_as_an_assignment`.
        {
            let sysfs = TempDir::new().unwrap();
            let gpu = sysfs.path().join("bus/pci/devices/0000:01:00.0");
            file(&gpu.join("class"), "0x030200\n");
            let kind = gpu.join("mdev_supported_types/nvidia-1");
            file(&kind.join("available_instances"), "2\n");
            file(&kind.join("device_api"), "vfio-pci\n");
            file(&kind.join("name"), "one display\n");
            fs::create_dir_all(kind.join("devices")).unwrap();
            let probe = GraphicsCapabilities::probe_sysfs(sysfs.path()).unwrap();
            assert_eq!(
                probe.recommendation,
                GraphicsRecommendation::PartitionCapableNotConfigured
            );
            assert!(
                probe.devices[0].mediated_types[0]
                    .created_devices
                    .is_empty()
            );
        }
        // Phase `only_an_iommu_isolated_vfio_bound_virtual_function_is_ready`.
        {
            let sysfs = TempDir::new().unwrap();
            let pci = sysfs.path().join("bus/pci/devices");
            let pf = pci.join("0000:03:00.0");
            let vf = pci.join("0000:03:00.1");
            file(&pf.join("class"), "0x030000\n");
            file(&pf.join("sriov_totalvfs"), "4\n");
            file(&pf.join("sriov_numvfs"), "1\n");
            file(&vf.join("class"), "0x030000\n");
            let driver = sysfs.path().join("bus/pci/drivers/vfio-pci");
            let group = sysfs.path().join("kernel/iommu_groups/12");
            fs::create_dir_all(&driver).unwrap();
            fs::create_dir_all(group.join("devices/0000:03:00.1")).unwrap();
            symlink(&vf, pf.join("virtfn0")).unwrap();
            symlink(&driver, vf.join("driver")).unwrap();
            symlink(&group, vf.join("iommu_group")).unwrap();
            let probe = GraphicsCapabilities::probe_sysfs(sysfs.path()).unwrap();
            assert_eq!(
                probe.recommendation,
                GraphicsRecommendation::PartitionReadyForExplicitAssignment
            );
            let parent = probe
                .devices
                .iter()
                .find(|device| device.pci_address == "0000:03:00.0")
                .unwrap();
            assert_eq!(
                parent.sriov.as_ref().unwrap().vfio_ready_vfs,
                ["0000:03:00.1"]
            );
            let assignment = probe.sriov_assignment("0000:03:00.1").unwrap();
            let xml = assignment.domain_xml().unwrap();
            assert!(xml.contains("domain='0x0000' bus='0x03' slot='0x00' function='0x1'"));
            assert!(xml.contains("managed='no'"));
            assert!(probe.sriov_assignment("0000:03:00.0").is_none());

            // Presence of an IOMMU group alone is insufficient: Tilo never offers
            // a VF whose group also contains a host device it would strand.
            fs::create_dir_all(group.join("devices/0000:00:1f.0")).unwrap();
            let shared_group_probe = GraphicsCapabilities::probe_sysfs(sysfs.path()).unwrap();
            assert!(
                shared_group_probe
                    .sriov_assignment("0000:03:00.1")
                    .is_none()
            );
        }
        // Phase `mediated_assignment_requires_an_existing_device_and_escapes_nothing`.
        {
            let sysfs = TempDir::new().unwrap();
            let gpu = sysfs.path().join("bus/pci/devices/0000:01:00.0");
            file(&gpu.join("class"), "0x030200\n");
            let kind = gpu.join("mdev_supported_types/vendor-display");
            file(&kind.join("available_instances"), "1\n");
            file(&kind.join("device_api"), "vfio-pci\n");
            let uuid = "53764d0e-85a0-42b4-af5c-2046b460b1dc";
            fs::create_dir_all(kind.join("devices")).unwrap();
            file(&kind.join("devices").join(uuid).join("remove"), "");
            let probe = GraphicsCapabilities::probe_sysfs(sysfs.path()).unwrap();
            let assignment = probe.mediated_assignment(uuid).unwrap();
            assert!(assignment.domain_xml().unwrap().contains(uuid));
            assert!(
                probe
                    .mediated_assignment("../../active-host-device")
                    .is_none()
            );
        }
        // Phase `non_display_sriov_devices_are_ignored`.
        {
            let sysfs = TempDir::new().unwrap();
            let nic = sysfs.path().join("bus/pci/devices/0000:04:00.0");
            file(&nic.join("class"), "0x020000\n");
            file(&nic.join("sriov_totalvfs"), "8\n");
            let probe = GraphicsCapabilities::probe_sysfs(sysfs.path()).unwrap();
            assert!(probe.devices.is_empty());
            assert_eq!(probe.recommendation, GraphicsRecommendation::Paravirtual2d);
        }
    }
}
