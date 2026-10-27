//! Serialized source descriptions are metadata, not a native source permission.
use super::*;

fn sample() -> Plan {
    Plan {
        schema: SCHEMA,
        operation_id: "migration-metadata".into(),
        categories: Categories {
            home: true,
            flatpak: false,
            accounts: false,
        },
        home_policy: HomePolicy::Copy,
        home_exclusions: Vec::new(),
        deferred_handoff: None,
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
            relative: NativePath::encode(std::path::Path::new("home/person")),
            inode: 1234,
            original_uid: 1000,
            subvolume: None,
        },
        flatpak_inventory: None,
    }
}

#[test]
fn source_metadata_round_trips_without_a_native_permission() {
    let plan = sample();
    assert_eq!(Plan::parse(&plan.bytes().unwrap()).unwrap(), plan);
    assert_eq!(plan.home_inode(), 1234);
    assert_eq!(plan.home_uid(), 1000);
}

#[test]
fn source_metadata_still_validates_geometry_and_consent() {
    let mut plan = sample();
    plan.home.volume.last_lba = u64::MAX;
    assert!(plan.bytes().is_err());
    let mut plan = sample();
    plan.categories.home = false;
    plan.home_policy = HomePolicy::AccessReadWrite;
    assert!(plan.bytes().is_err());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn native_application_inventory_is_not_silently_accepted() {
    let mut plan = sample();
    plan.categories.flatpak = true;
    plan.flatpak_inventory = Some(serde_json::json!({}));
    assert_eq!(plan.bytes().unwrap_err().kind(), io::ErrorKind::Unsupported);
}

#[test]
fn deferred_handoff_keeps_released_plans_and_binds_the_same_uid_1000_source() {
    let mut plan = sample();
    let released = plan.bytes().unwrap();
    assert!(
        !serde_json::from_slice::<serde_json::Value>(&released)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("deferred_handoff")
    );
    let base = plan.base_digest().unwrap();
    plan.attach_deferred_handoff(DeferredHandoff {
        schema: 1,
        original_uid: DEFERRED_ORIGINAL_UID,
        journal_sha256: [7; 32],
    })
    .unwrap();
    assert_eq!(plan.base_digest().unwrap(), base);
    let parsed = Plan::parse(&plan.bytes().unwrap()).unwrap();
    assert_eq!(parsed.deferred_handoff, plan.deferred_handoff);
    for handoff in [
        DeferredHandoff {
            schema: 2,
            original_uid: DEFERRED_ORIGINAL_UID,
            journal_sha256: [7; 32],
        },
        DeferredHandoff {
            schema: 1,
            original_uid: 1001,
            journal_sha256: [7; 32],
        },
        DeferredHandoff {
            schema: 1,
            original_uid: DEFERRED_ORIGINAL_UID,
            journal_sha256: [0; 32],
        },
    ] {
        let mut bad = sample();
        bad.deferred_handoff = Some(handoff);
        assert!(bad.bytes().is_err());
    }
}
