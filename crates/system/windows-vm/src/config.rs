use std::path::{Path, PathBuf};

use thiserror::Error;

pub const APPLIANCE_DOMAIN_NAME: &str = "qos-windows-appliance";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuMode {
    /// Lowest latency and the Windows Hyper-V enlightenments. This appliance is
    /// local state and is never live-migrated.
    HostPassthrough,
    /// A named libvirt CPU model for unusual hosts which cannot pass through.
    HostModel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareConfig {
    pub secure_boot_loader: PathBuf,
    pub nvram_template: PathBuf,
    pub nvram: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PerformanceConfig {
    pub memory_mib: u32,
    pub vcpus: u16,
    pub io_threads: u16,
    pub cpu_mode: CpuMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplianceConfig {
    pub domain_name: String,
    pub domain_uuid: String,
    pub system_disk: PathBuf,
    pub firmware: FirmwareConfig,
    pub performance: PerformanceConfig,
    /// A libvirt network with forwarding disabled. Guest internet access is
    /// opened by the qOS capability broker, never by changing this NIC.
    pub host_only_network: String,
    /// Fixed `AF_VSOCK` context identifier used only by the authenticated guest bridge.
    pub guest_cid: u32,
}

impl ApplianceConfig {
    #[must_use]
    pub fn recommended(
        domain_uuid: impl Into<String>,
        system_disk: impl Into<PathBuf>,
        nvram: impl Into<PathBuf>,
    ) -> Self {
        Self {
            domain_name: APPLIANCE_DOMAIN_NAME.to_string(),
            domain_uuid: domain_uuid.into(),
            system_disk: system_disk.into(),
            firmware: FirmwareConfig {
                secure_boot_loader: PathBuf::from("/usr/share/edk2/ovmf/OVMF_CODE.secboot.fd"),
                nvram_template: PathBuf::from("/usr/share/edk2/ovmf/OVMF_VARS.fd"),
                nvram: nvram.into(),
            },
            performance: PerformanceConfig {
                memory_mib: 8192,
                vcpus: 4,
                io_threads: 1,
                cpu_mode: CpuMode::HostPassthrough,
            },
            host_only_network: "qos-windows-hostonly".to_string(),
            guest_cid: 7,
        }
    }

    /// Validates every string that crosses into libvirt and enforces the
    /// singleton identity. Callers cannot create a second Windows domain by
    /// choosing a different name.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.domain_name != APPLIANCE_DOMAIN_NAME {
            return Err(ConfigError::NotSingleton(self.domain_name.clone()));
        }
        if !is_uuid(&self.domain_uuid) {
            return Err(ConfigError::InvalidUuid);
        }
        validate_absolute("system disk", &self.system_disk)?;
        validate_absolute("secure boot loader", &self.firmware.secure_boot_loader)?;
        validate_absolute("NVRAM template", &self.firmware.nvram_template)?;
        validate_absolute("NVRAM", &self.firmware.nvram)?;
        if self.performance.memory_mib < 4096 {
            return Err(ConfigError::InsufficientMemory);
        }
        if self.performance.vcpus == 0 || self.performance.vcpus > 64 {
            return Err(ConfigError::InvalidVcpuCount);
        }
        if self.performance.io_threads == 0 || self.performance.io_threads > 16 {
            return Err(ConfigError::InvalidIoThreadCount);
        }
        if !is_libvirt_name(&self.host_only_network) {
            return Err(ConfigError::InvalidNetworkName);
        }
        if self.guest_cid < 3 {
            return Err(ConfigError::ReservedGuestCid);
        }
        Ok(())
    }
}

fn validate_absolute(label: &'static str, path: &Path) -> Result<(), ConfigError> {
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return Err(ConfigError::PathNotAbsolute(label));
    }
    Ok(())
}

fn is_libvirt_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn is_uuid(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }
    value.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    })
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ConfigError {
    #[error("the Windows fallback must use the singleton domain name, got {0:?}")]
    NotSingleton(String),
    #[error("the configured domain UUID is not a canonical UUID")]
    InvalidUuid,
    #[error("{0} must be an absolute host path")]
    PathNotAbsolute(&'static str),
    #[error("the appliance requires at least 4096 MiB")]
    InsufficientMemory,
    #[error("vCPU count must be between 1 and 64")]
    InvalidVcpuCount,
    #[error("I/O thread count must be between 1 and 16")]
    InvalidIoThreadCount,
    #[error("the host-only libvirt network name is invalid")]
    InvalidNetworkName,
    #[error("AF_VSOCK context identifiers 0, 1, and 2 are reserved")]
    ReservedGuestCid,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ApplianceConfig {
        ApplianceConfig::recommended(
            "01234567-89ab-cdef-0123-456789abcdef",
            "/var/lib/qos/windows/system.qcow2",
            "/var/lib/qos/windows/nvram.fd",
        )
    }

    #[test]
    fn recommended_configuration_is_valid() {
        config().validate().unwrap();
    }

    #[test]
    fn a_caller_cannot_define_a_second_domain() {
        let mut alternate = config();
        alternate.domain_name = "my-other-windows".to_string();
        assert!(matches!(
            alternate.validate(),
            Err(ConfigError::NotSingleton(_))
        ));
    }

    #[test]
    fn paths_must_be_absolute() {
        let mut relative = config();
        relative.system_disk = PathBuf::from("disk.qcow2");
        assert_eq!(
            relative.validate(),
            Err(ConfigError::PathNotAbsolute("system disk"))
        );
    }
}
