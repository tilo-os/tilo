//! The frontend's staging record retains only the private source plan.
use super::*;
use crate::setup::migration_store::NativePath;
use std::path::Path;

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

#[test]
fn staging_record_retains_source_plan_privately_and_binds_installation_geometry() {
    use crate::setup::staging::{plan, platform::Partition, record};
    let mut migration = sample();
    migration.home.volume.disk_bytes = 256 << 30;
    let basis = plan::tests::linux_basis(
        512,
        migration.home.volume.disk_bytes,
        vec![Partition {
            number: 1,
            guid: migration.home.volume.partition_guid.clone(),
            type_guid: "0fc63daf-8483-4772-8e79-3d69d8477de4".into(),
            first_lba: migration.home.volume.first_lba,
            last_lba: migration.home.volume.last_lba,
        }],
    );
    migration.home.volume.disk_guid = basis.disk.guid.clone();
    let installation = plan::compute(
        &basis,
        plan::Requirements {
            staging_esp_bytes: 600 << 20,
            staging_root_bytes: 40 << 30,
            target_bytes: 64 << 30,
        },
    )
    .unwrap();
    migration.validate_for_install(&installation).unwrap();
    let mut record = record::Record::new(
        migration.operation_id.clone(),
        installation,
        record::ReleaseRef {
            release_id: "fixture".into(),
            manifest_sha256: "a".repeat(64),
            architecture: "x86_64".into(),
        },
        record::OriginalFirmware {
            boot_order: vec![],
            boot_next: None,
            entries: std::collections::BTreeMap::default(),
        },
    )
    .unwrap();
    record.migration = Some(serde_json::to_value(&migration).unwrap());
    record.validate().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let store = record::RecordStore::at(temp.path());
    store.save(&record).unwrap();
    assert_eq!(
        store
            .load(&migration.operation_id)
            .unwrap()
            .unwrap()
            .migration,
        record.migration
    );
    let public = std::fs::read(
        store
            .public_dir
            .as_ref()
            .unwrap()
            .join(format!("{}.json", migration.operation_id)),
    )
    .unwrap();
    let public: serde_json::Value = serde_json::from_slice(&public).unwrap();
    assert!(public.get("migration").is_none());
    migration.home.volume.last_lba = record.plan.target_extent.first_lba;
    assert!(migration.validate_for_install(&record.plan).is_err());
    record.migration = Some(serde_json::to_value(&migration).unwrap());
    assert!(record.validate().is_err());
}
