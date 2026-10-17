//! Synthetic source metadata for tests; never a native source grant.
use super::*;

pub(super) fn sample() -> Plan {
    Plan {
        schema: SCHEMA,
        flatpak_inventory: None,
        operation_id: "migration-1".into(),
        categories: Categories {
            home: true,
            flatpak: true,
            accounts: true,
        },
        home_policy: HomePolicy::Copy,
        home: Home {
            volume: SourceVolume {
                disk_guid: "11111111-1111-4111-8111-111111111111".into(),
                disk_bytes: 64 << 30,
                sector_bytes: 512,
                partition_guid: "22222222-2222-4222-8222-222222222222".into(),
                first_lba: 2048,
                last_lba: 1048575,
                filesystem_uuid: "33333333-3333-4333-8333-333333333333".into(),
                filesystem: Filesystem::Ext4,
            },
            relative: NativePath::encode(Path::new("home/person")),
            inode: 1234,
            original_uid: 1000,
            subvolume: None,
        },
    }
}
