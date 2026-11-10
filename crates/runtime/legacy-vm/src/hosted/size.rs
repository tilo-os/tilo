//! The sizes a hosted VM comes in.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A hosted VM's machine and disk. For a Linux VM the disk is persistent:
/// it outlives every suspend and stop, and only deleting the VM frees it.
///
/// [`Self::Browser`] is the one size that is not a Linux VM: it boots the
/// browser guest image instead (a host boots one image per size class), it
/// is never snapshotted, and what it writes is deleted when it stops. It is
/// asked for with [`Request::BrowserOpen`](super::control::Request) by the
/// relay, never by a shell, and it is chosen so that the placer and the
/// memory budget weigh it like any other VM.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum VmSize {
    /// 2 vCPU, 4 GiB, 50 GiB disk.
    Small,
    /// 4 vCPU, 8 GiB, 50 GiB disk.
    #[default]
    Default,
    /// 8 vCPU, 16 GiB, 110 GiB disk.
    Large,
    /// 16 vCPU, 32 GiB, 250 GiB disk.
    Xlarge,
    /// A browser environment: 2 vCPU, 2 GiB, 8 GiB disk, kept for nothing.
    Browser,
}

impl VmSize {
    pub const ALL: [Self; 5] = [
        Self::Small,
        Self::Default,
        Self::Large,
        Self::Xlarge,
        Self::Browser,
    ];

    /// The sizes a Linux VM is asked for in: every size but the browser's.
    pub const LINUX: [Self; 4] = [Self::Small, Self::Default, Self::Large, Self::Xlarge];

    #[must_use]
    pub const fn vcpus(self) -> u32 {
        match self {
            Self::Small | Self::Browser => 2,
            Self::Default => 4,
            Self::Large => 8,
            Self::Xlarge => 16,
        }
    }

    #[must_use]
    pub const fn memory_mib(self) -> u64 {
        match self {
            Self::Small => 4 * 1024,
            Self::Browser => 2 * 1024,
            Self::Default => 8 * 1024,
            Self::Large => 16 * 1024,
            Self::Xlarge => 32 * 1024,
        }
    }

    #[must_use]
    pub const fn disk_gib(self) -> u64 {
        match self {
            Self::Small | Self::Default => 50,
            Self::Browser => 8,
            Self::Large => 110,
            Self::Xlarge => 250,
        }
    }

    #[must_use]
    pub const fn disk_bytes(self) -> u64 {
        self.disk_gib().saturating_mul(1 << 30)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Default => "default",
            Self::Large => "large",
            Self::Xlarge => "xlarge",
            Self::Browser => "browser",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|size| size.as_str() == name)
    }

    /// `default (4 vCPU, 8 GB, 50 GB disk)`: how a person reads a size.
    #[must_use]
    pub fn describe(self) -> String {
        format!(
            "{} ({} vCPU, {} GB, {} GB disk)",
            self.as_str(),
            self.vcpus(),
            self.memory_mib() / 1024,
            self.disk_gib()
        )
    }
}

impl fmt::Display for VmSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_published_table() {
        let table: Vec<_> = VmSize::ALL
            .iter()
            .map(|size| {
                (
                    size.as_str(),
                    size.vcpus(),
                    size.memory_mib(),
                    size.disk_gib(),
                )
            })
            .collect();
        assert_eq!(
            table,
            [
                ("small", 2, 4096, 50),
                ("default", 4, 8192, 50),
                ("large", 8, 16384, 110),
                ("xlarge", 16, 32768, 250),
                ("browser", 2, 2048, 8),
            ]
        );
        assert_eq!(VmSize::LINUX.len() + 1, VmSize::ALL.len());
        for size in VmSize::ALL {
            assert_eq!(VmSize::parse(size.as_str()), Some(size));
            let json = serde_json::to_string(&size).unwrap();
            assert_eq!(json, format!("\"{size}\""));
        }
        assert_eq!(VmSize::parse("huge"), None);
        assert_eq!(
            VmSize::Large.describe(),
            "large (8 vCPU, 16 GB, 110 GB disk)"
        );
    }
}
