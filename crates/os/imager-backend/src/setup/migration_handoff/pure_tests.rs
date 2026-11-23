//! Serialized source descriptions are metadata, not a native source permission.
use super::*;

fn sample() -> Plan {
    Plan(Migration {
        schema: SCHEMA,
        operation_id: "migration-metadata".into(),
        categories: Categories {
            home: true,
            flatpak: false,
            accounts: false,
        },
        home_policy: HomePolicy::Copy,
        home: Home::new(
            SourceVolume {
                disk_guid: "11111111-1111-4111-8111-111111111111".into(),
                disk_bytes: 64 << 30,
                sector_bytes: 512,
                partition_guid: "22222222-2222-4222-8222-222222222222".into(),
                first_lba: 2048,
                last_lba: 1048575,
                filesystem_uuid: "33333333-3333-4333-8333-333333333333".into(),
                filesystem: Filesystem::Ext4,
            },
            NativePath::encode(std::path::Path::new("home/person")),
            1234,
            1000,
            None,
        ),
        pending_computer_applications_scope: None,
        flatpak_inventory: None,
        home_exclusions: Vec::new(),
        deferred_handoff: None,
    })
}

tilo_test_env::scenario! {
    /// A handoff plan's source metadata.
    /// Each phase was a test of its own and builds its own fixture.
    fn source_metadata_round_trips_and_still_validates_geometry_and_consent;

    phase source_metadata_round_trips_without_a_native_permission;
    {
        let plan = sample();
        assert_eq!(Plan::parse(&plan.bytes().unwrap()).unwrap(), plan);
        assert_eq!(plan.home_inode(), 1234);
        assert_eq!(plan.home_uid(), 1000);
    }

    phase source_metadata_still_validates_geometry_and_consent;
    {
        let mut plan = sample();
        plan.home.volume.last_lba = u64::MAX;
        assert!(plan.bytes().is_err());
        let mut plan = sample();
        plan.categories.home = false;
        plan.home_policy = HomePolicy::AccessReadWrite;
        assert!(plan.bytes().is_err());
    }
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

#[test]
fn plan_with_home_exclusions_crosses_to_staging_migration() {
    let mut plan = sample();
    plan.operation_id = "0f0e0d0c-0b0a-4908-8706-050403020100".into();
    plan.home_exclusions = vec![NativePath::encode(std::path::Path::new(
        "Documents/Private",
    ))];
    let bytes = plan.bytes().unwrap();
    let migration: staging_handoff_model::Migration = serde_json::from_slice(&bytes).unwrap();
    migration.validate_standalone().unwrap();
    let round_tripped = Plan::parse(&serde_json::to_vec(&migration).unwrap()).unwrap();
    assert_eq!(round_tripped, plan);
}

#[test]
fn migration_with_pending_computer_applications_scope_reads_as_plan() {
    let source_operation = "0f0e0d0c-0b0a-4908-8706-050403020100";
    let raw = serde_json::json!({
        "schema": 1,
        "operation_id": source_operation,
        "categories": {"home": true, "flatpak": false, "accounts": false},
        "home_policy": "access-read-write",
        "home": {
            "volume": {
                "disk_guid": "11111111-1111-4111-8111-111111111111",
                "disk_bytes": 64u64 << 30,
                "sector_bytes": 512,
                "partition_guid": "22222222-2222-4222-8222-222222222222",
                "first_lba": 2048,
                "last_lba": 1048575,
                "filesystem_uuid": "33333333-3333-4333-8333-333333333333",
                "filesystem": "ext4"
            },
            "relative": {"platform": "Unix", "units": b"home/person"},
            "inode": 1234,
            "original_uid": 1000,
            "subvolume": null
        },
        "pending_computer_applications_scope": {
            "schema": 1,
            "source_operation": source_operation,
            "applications_operation": "44444444-4444-4444-8444-444444444444",
            "staging_operation": "55555555-5555-4555-8555-555555555555",
            "staging_plan_sha256": "a".repeat(64),
            "setup_binding": "owned setup fixture",
            "identity_public_key": vec![7u8; 32]
        }
    });
    let migration: staging_handoff_model::Migration = serde_json::from_value(raw.clone()).unwrap();
    migration.validate_standalone().unwrap();
    let bytes = serde_json::to_vec(&migration).unwrap();
    let plan = Plan::parse(&bytes).unwrap();
    let reencoded: serde_json::Value = serde_json::from_slice(&plan.bytes().unwrap()).unwrap();
    assert_eq!(reencoded, raw);

    // Unknown fields and malformed pending scopes remain refused on the shared schema.
    let mut extra = raw.clone();
    extra["unexpected"] = serde_json::json!(true);
    assert!(Plan::parse(&serde_json::to_vec(&extra).unwrap()).is_err());

    let mut extra_scope = raw.clone();
    extra_scope["pending_computer_applications_scope"]["unexpected"] = serde_json::json!(true);
    assert!(Plan::parse(&serde_json::to_vec(&extra_scope).unwrap()).is_err());

    let mut null_scope = raw.clone();
    null_scope["pending_computer_applications_scope"] = serde_json::Value::Null;
    assert!(Plan::parse(&serde_json::to_vec(&null_scope).unwrap()).is_err());

    let mut mismatched = raw;
    mismatched["categories"]["flatpak"] = serde_json::json!(true);
    assert!(Plan::parse(&serde_json::to_vec(&mismatched).unwrap()).is_err());

    // TryFrom<Migration> refuses unvalidated migrations.
    let mut invalid_migration: Migration = sample().into();
    invalid_migration.schema = 99;
    assert!(Plan::try_from(invalid_migration).is_err());
}

#[test]
fn plan_parse_refuses_repeated_key_inside_flatpak_inventory() {
    let mut plan = sample();
    plan.categories.flatpak = true;
    plan.flatpak_inventory = Some(serde_json::json!({
        "schema": 1,
        "inventory": {"refs": [], "remotes": []},
        "selected": [],
        "installations": []
    }));
    let json = serde_json::to_string(&plan).unwrap();
    let repeated = json.replacen(
        "\"flatpak_inventory\":{\"schema\":1",
        "\"flatpak_inventory\":{\"schema\":1,\"schema\":2",
        1,
    );
    assert_ne!(json, repeated);
    assert!(serde_json::from_str::<Plan>(&repeated).is_err());
    assert!(Plan::parse(repeated.as_bytes()).is_err());
}
