//! UI-free image and removable-drive contracts for native media actors.

/// Authenticated GPT expectation carried across the native media boundary.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaPartitionBinding {
    pub schema: u32,
    pub target_stable_id: String,
    pub disk_guid: String,
    pub partition_guid: String,
    pub number: u32,
    pub first_lba: u64,
    pub last_lba: u64,
    pub sector_bytes: u64,
    pub profile_offset: u64,
}

/// Fixed profile gap shared by capacity planning and the native writer.
pub const PROFILE_BLOCK_SIZE: usize = 64 * 1024;

// Compatibility names refer to the canonical frontend/native DTOs.
pub use crate::imager_model::{
    Source, SourceIdentity, SourceKind, TargetDrive, TargetKind, TargetPartition,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_eligibility_requires_external_facts_and_always_excludes_system_disks() {
        for kind in [
            TargetKind::Usb,
            TargetKind::SdCard,
            TargetKind::Nvme,
            TargetKind::Ssd,
        ] {
            for removable in [false, true] {
                for transport in [
                    None,
                    Some("usb"),
                    Some("USB"),
                    Some("sd"),
                    Some("mmc"),
                    Some("sata"),
                    Some("nvme"),
                    Some(""),
                ] {
                    for is_system in [false, true] {
                        let target = TargetDrive {
                            name: "test-drive".into(),
                            model: String::new(),
                            stable_id: None,
                            size: 1024,
                            kind,
                            tran: transport.map(str::to_owned),
                            removable,
                            is_system,
                            partitions: Vec::new(),
                        };
                        let external =
                            removable || matches!(transport, Some("usb" | "USB" | "sd" | "mmc"));
                        assert_eq!(
                            target.is_media_target(),
                            external && !is_system,
                            "kind={kind:?}, removable={removable}, transport={transport:?}, system={is_system}"
                        );
                    }
                }
            }
        }
    }
}
