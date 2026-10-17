use super::*;

pub(super) use super::fixture::sample;

#[test]
fn metadata_plan_round_trips_and_rejects_secrets_and_path_traversal() {
    let plan = sample();
    assert_eq!(Plan::parse(&plan.bytes().unwrap()).unwrap(), plan);
    let mut different_consent = plan.clone();
    different_consent.categories.accounts = false;
    assert_ne!(plan.digest().unwrap(), different_consent.digest().unwrap());
    let mut raw = serde_json::to_value(&plan).unwrap();
    raw["credentials"] = "never acceptable".into();
    assert!(Plan::parse(&serde_json::to_vec(&raw).unwrap()).is_err());
    let mut bad = plan;
    for path in ["/home/person", "home/../person", ""] {
        bad.home.relative = NativePath::encode(Path::new(path));
        assert!(bad.bytes().is_err(), "{path}");
    }
}

#[cfg(unix)]
#[test]
fn durable_plan_is_private_immutable_and_does_not_adopt_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir().unwrap();
    let private = temp.path().canonicalize().unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    let plan = sample();
    let path = plan.write_private(&private).unwrap();
    assert_eq!(
        Plan::read_private(&private, &plan.operation_id).unwrap(),
        plan
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(plan.write_private(&private).is_err());
    std::fs::remove_file(&path).unwrap();
    let outside = private.join("outside");
    std::fs::write(&outside, plan.bytes().unwrap()).unwrap();
    symlink(&outside, &path).unwrap();
    assert!(Plan::read_private(&private, &plan.operation_id).is_err());
    assert_eq!(std::fs::read(outside).unwrap(), plan.bytes().unwrap());
}

#[test]
fn retained_policy_is_explicit_and_legacy_plans_remain_copy() {
    let legacy = sample();
    let bytes = legacy.bytes().unwrap();
    assert!(
        !serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("home_policy")
    );
    assert_eq!(Plan::parse(&bytes).unwrap().home_policy, HomePolicy::Copy);
    let mut selected = legacy;
    selected.home_policy = HomePolicy::AccessReadWrite;
    assert_eq!(
        Plan::parse(&selected.bytes().unwrap()).unwrap().home_policy,
        HomePolicy::AccessReadWrite
    );
    selected.categories.home = false;
    assert!(selected.validate().is_err());
}

/// Synthetic public metadata only; no source mount, native grant, namespace,
/// credentials, deployment or fabricated original-caller authority is admitted.
#[cfg(target_os = "linux")]
fn original_inventory_codec_sample(remote_count: usize) -> Plan {
    let mut plan = sample();
    let mut root = plan.clone();
    root.categories = Categories {
        home: false,
        flatpak: true,
        accounts: false,
    };
    let remotes: Vec<_> = (0..remote_count)
        .map(|index| {
            serde_json::json!({
                "installation":{"System":"system"},"name":format!("public-{index}"),
                "url":format!("https://example.invalid/{}", "p".repeat(128)),"options":""
            })
        })
        .collect();
    plan.flatpak_inventory = Some(serde_json::json!({
        "schema":1,"inventory":{"refs":[],"remotes":remotes},"selected":[],
        "installations":[{"installation":{"System":"system"},"source_plan":root.bytes().unwrap(),
            "repositories":[],"deployments":[],"config_sha256":vec![7u8;32]}]
    }));
    plan
}

#[cfg(target_os = "linux")]
#[test]
fn protected_inventory_codec_refuses_shape_operation_and_content_substitutions() {
    use sha2::{Digest, Sha256};
    let inline = original_inventory_codec_sample(1);
    let (compact, bytes) = inline.original_inventory_selection().unwrap();
    let bytes = bytes.unwrap();
    let encoded = compact.bytes().unwrap();
    let decoded = Plan::parse(&encoded).unwrap();
    assert_eq!(decoded, compact);
    let binding = OriginalInventoryBinding::parse(&decoded).unwrap().unwrap();
    binding.validate_bytes(&bytes).unwrap();
    assert_eq!(
        inline.applications_copy_plan().unwrap().flatpak_inventory,
        inline.flatpak_inventory
    );
    assert!(
        decoded.applications_copy_plan().is_err(),
        "marker is no ordinary copy inventory"
    );
    assert!(
        decoded.original_inventory_selection().is_err(),
        "marker alone cannot supply registration bytes"
    );
    assert!(
        crate::setup::flatpak_transfer::HandoffInventory::parse(
            &serde_json::to_vec(decoded.flatpak_inventory.as_ref().unwrap()).unwrap()
        )
        .is_err()
    );

    let wire = serde_json::to_value(&decoded).unwrap();
    for (field, value) in [
        ("kind", serde_json::json!("untrusted-path")),
        ("schema", serde_json::json!(2)),
        ("operation", serde_json::json!("unrelated-operation")),
        ("len", serde_json::json!(0)),
        ("len", serde_json::json!(INVENTORY_BOUND + 1)),
        ("sha256", serde_json::json!(vec![0u8; 32])),
        ("path", serde_json::json!("/unapproved/inventory")),
    ] {
        let mut changed = wire.clone();
        changed["flatpak_inventory"][field] = value;
        assert!(
            Plan::parse(&serde_json::to_vec(&changed).unwrap()).is_err(),
            "{field}"
        );
    }
    let mut no_consent = wire.clone();
    no_consent["categories"]["flatpak"] = serde_json::json!(false);
    assert!(Plan::parse(&serde_json::to_vec(&no_consent).unwrap()).is_err());
    // Shape-valid lies still refuse actual bytes; parser shape isn't authority.
    for (field, value) in [
        ("len", serde_json::json!(binding.len + 1)),
        ("sha256", serde_json::json!(vec![19u8; 32])),
    ] {
        let mut changed = wire.clone();
        changed["flatpak_inventory"][field] = value;
        let changed = Plan::parse(&serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(
            OriginalInventoryBinding::parse(&changed)
                .unwrap()
                .unwrap()
                .validate_bytes(&bytes)
                .is_err()
        );
    }
    // Even a correctly hashed canonical Handoff cannot substitute its embedded
    // installation operation while reusing the outer original source binding.
    let mut inventory: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let root_bytes: Vec<u8> =
        serde_json::from_value(inventory["installations"][0]["source_plan"].clone()).unwrap();
    let mut root = Plan::parse(&root_bytes).unwrap();
    root.operation_id = "different-operation".into();
    inventory["installations"][0]["source_plan"] = serde_json::json!(root.bytes().unwrap());
    let canonical = crate::setup::flatpak_transfer::HandoffInventory::parse(
        &serde_json::to_vec(&inventory).unwrap(),
    )
    .unwrap()
    .bytes()
    .unwrap();
    let mut changed = wire.clone();
    changed["flatpak_inventory"]["len"] = serde_json::json!(canonical.len());
    changed["flatpak_inventory"]["sha256"] =
        serde_json::json!(<[u8; 32]>::from(Sha256::digest(&canonical)).to_vec());
    let changed = Plan::parse(&serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(
        OriginalInventoryBinding::parse(&changed)
            .unwrap()
            .unwrap()
            .validate_bytes(&canonical)
            .is_err()
    );

    // Whitespace-altered bytes with an updated length/hash still refuse:
    // canonical inventory custody cannot ambiguously authenticate two encodings.
    let mut padded = bytes;
    padded.push(b'\n');
    let mut changed = wire;
    changed["flatpak_inventory"]["len"] = serde_json::json!(padded.len());
    changed["flatpak_inventory"]["sha256"] =
        serde_json::json!(<[u8; 32]>::from(Sha256::digest(&padded)).to_vec());
    let changed = Plan::parse(&serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(
        OriginalInventoryBinding::parse(&changed)
            .unwrap()
            .unwrap()
            .validate_bytes(&padded)
            .is_err()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn protected_inventory_codec_preserves_ordinary_64k_plan_limit_without_truncation() {
    let inline = original_inventory_codec_sample(1024);
    inline.validate().unwrap();
    let ordinary_wire = serde_json::to_vec(&inline).unwrap();
    assert!(ordinary_wire.len() > MAX_BYTES);
    assert!(inline.bytes().is_err());
    assert!(
        Plan::parse(&ordinary_wire).is_err(),
        "ordinary oversized plan cannot cross its codec boundary"
    );
    let (compact, bytes) = inline.original_inventory_selection().unwrap();
    let bytes = bytes.unwrap();
    assert!(bytes.len() > MAX_BYTES);
    assert!(bytes.len() <= INVENTORY_BOUND as usize);
    let parsed = crate::setup::flatpak_transfer::HandoffInventory::parse(&bytes).unwrap();
    assert_eq!(
        serde_json::to_value(parsed).unwrap(),
        inline.flatpak_inventory.unwrap(),
        "every source and public record survives normalization"
    );
    let compact_wire = compact.bytes().unwrap();
    assert!(compact_wire.len() < MAX_BYTES);
    let decoded = Plan::parse(&compact_wire).unwrap();
    OriginalInventoryBinding::parse(&decoded)
        .unwrap()
        .unwrap()
        .validate_bytes(&bytes)
        .unwrap();
    assert!(
        decoded.applications_copy_plan().is_err(),
        "large inventory marker doesn't authorize ordinary copy"
    );
    // A small ordinary inline plan keeps its existing interoperable encoding.
    let ordinary = original_inventory_codec_sample(1);
    assert_eq!(Plan::parse(&ordinary.bytes().unwrap()).unwrap(), ordinary);
    ordinary.applications_copy_plan().unwrap();
}
