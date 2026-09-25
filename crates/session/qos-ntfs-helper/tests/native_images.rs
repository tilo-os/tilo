#![cfg(unix)]
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, process::Command};
fn fixtures() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(PathBuf::from(
        std::env::var_os("QOS_NTFS_FIXTURES")
            .ok_or("generate fixtures and set QOS_NTFS_FIXTURES")?,
    ))
}
fn probe(name: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let path = fixtures()?.join(format!("{name}.img"));
    let before = Sha256::digest(fs::read(&path)?);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(fixtures()?.join("manifest.json"))?)?;
    let expected = manifest
        .get("images")
        .and_then(|v| v.get(format!("{name}.img")))
        .and_then(|v| v.get("sha256"))
        .and_then(serde_json::Value::as_str)
        .ok_or("missing fixture checksum")?;
    if hex::encode(before) != expected {
        return Err("fixture does not match generated manifest".into());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_qos-ntfs-image-test"))
        .arg(&path)
        .output()?;
    let unchanged = before == Sha256::digest(fs::read(&path)?);
    if !unchanged {
        return Err("complete image changed".into());
    }
    if !output.status.success() {
        return Err(format!("probe failed: {output:?}").into());
    }
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    if report
        .get("stats")
        .and_then(|s| s.get("writes"))
        .and_then(serde_json::Value::as_u64)
        != Some(0)
    {
        return Err("write callback attempted".into());
    }
    Ok(report)
}
#[test]
fn native_clean_image_returns_serial_root_generation_and_content_metadata() {
    let r = probe("clean").unwrap();
    assert_eq!(r["status"], 0, "{r}");
    assert_eq!(r["diagnostic_only"], true);
    let facts = r["facts"].as_array().expect("facts");
    assert!(
        facts
            .iter()
            .any(|v| v["kind"] == "volume" && v["serial"] == "1122334455667788")
    );
    assert!(
        facts.iter().any(|v| v["kind"] == "entry"
            && v["record"] == 5
            && v["sequence"].as_u64().unwrap() > 0)
    );
    assert!(
        facts.iter().any(|v| v["kind"] == "chunk"
            && v["hex"] == "436f6e6e6563746564204e54465320666978747572650a")
    );
}
#[test]
fn native_dirty_log_and_volume_flags_refuse() {
    assert_eq!(probe("dirty").unwrap()["status"], 3);
}
#[test]
fn native_hiber_absent_zero_nonzero_and_short() {
    assert_eq!(probe("zero-hiber").unwrap()["status"], 0);
    for name in ["hiber", "nonzero-hiber", "short-hiber"] {
        assert_eq!(probe(name).unwrap()["status"], 4, "{name}");
    }
}
#[test]
fn native_bitlocker_signature_is_locked() {
    assert_eq!(probe("bitlocker").unwrap()["status"], 2);
}
#[test]
fn native_probe_preserves_complete_image_bytes_on_success_and_failure() {
    for name in [
        "clean",
        "dirty",
        "zero-hiber",
        "hiber",
        "nonzero-hiber",
        "short-hiber",
        "bitlocker",
        "truncated",
        "corrupt",
    ] {
        let _ = probe(name).unwrap();
    }
}

#[test]
fn native_mft_generation_change_changes_identity() {
    let r = probe("generation").unwrap();
    assert_eq!(r["status"], 0, "{r}");
    assert!(
        r["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "entry" && v["record"] == 5 && v["sequence"] == 42)
    );
}
#[test]
fn native_named_directory_stream_and_reparse_are_reported() {
    let r = probe("metadata").unwrap();
    assert_eq!(r["status"], 0, "{r}");
    let f = r["facts"].as_array().unwrap();
    assert!(f.iter().any(|v| v["kind"] == "attribute"
        && v["type"] == 128
        && v["record"] == 5
        && v["name"] == "640069007200"));
    assert!(
        f.iter()
            .any(|v| v["kind"] == "attribute" && v["type"] == 192)
    );
}
#[test]
fn native_upcase_keys_follow_volume_table() {
    let r = probe("upcase").unwrap();
    assert_eq!(r["status"], 0, "{r}");
    assert!(
        r["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "entry"
                && v["name"] == "e4002e00740078007400"
                && v["key"] == "d6002e00540058005400")
    );
}
#[test]
fn native_oversize_is_bounded() {
    assert_eq!(probe("oversize").unwrap()["status"], 6);
}
#[test]
fn native_invalid_log_refuses() {
    assert_ne!(probe("badlog").unwrap()["status"], 0);
}
#[test]
fn native_read_budget_failure_is_explicit_and_nonmutating() {
    let path = fixtures().unwrap().join("clean.img");
    let before = Sha256::digest(fs::read(&path).unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_qos-ntfs-image-test"))
        .arg(&path)
        .args(["--read-limit", "512"])
        .output()
        .unwrap();
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["status"], 6);
    assert_eq!(before, Sha256::digest(fs::read(&path).unwrap()));
}

#[test]
fn native_hiber_io_error_refuses_without_mutation() {
    assert_ne!(probe("hiber-io").unwrap()["status"], 0);
}
#[test]
fn native_attribute_list_cycle_is_bounded() {
    assert_ne!(probe("attrlist-cycle").unwrap()["status"], 0);
}
#[test]
fn native_identity_binds_root_sequence_and_native_upcase_digest() {
    let a = probe("clean").unwrap();
    let b = probe("generation").unwrap();
    let c = probe("upcase").unwrap();
    assert!(
        a["native_identity"]["volume"].is_string(),
        "missing native identity"
    );
    assert_eq!(
        a["native_identity"]["volume"],
        b["native_identity"]["volume"]
    );
    assert_ne!(a["native_identity"]["root"], b["native_identity"]["root"]);
    assert_ne!(
        a["native_identity"]["upcase_sha256"],
        c["native_identity"]["upcase_sha256"]
    );
}
#[test]
fn native_security_descriptors_have_native_provenance() {
    let clean = probe("clean").unwrap();
    assert!(
        clean["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "security" && v["record"] == 5 && v["source"] == "attribute")
    );
    let acl = probe("acl").unwrap();
    assert!(
        acl["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "security" && v["record"] == 64 && v["source"] == "sds")
    );
}
#[test]
fn native_hardlink_resident_attributes_remain_distinct() {
    let r = probe("hardlink").unwrap();
    assert_eq!(r["status"], 0, "{r}");
    assert!(r["facts"].as_array().unwrap().iter().any(|v| {
        v["kind"] == "chunk"
            && v["attribute"] == 48
            && v["record"] == 64
            && v["hex"]
                .as_str()
                .is_some_and(|s| s.ends_with("7300650063006f006e006400"))
    }));
}
#[test]
fn native_folder_identity_rejects_a_regular_file_mft_locator() {
    let report: qos_ntfs_helper::protocol::Report =
        serde_json::from_value(probe("clean").unwrap()).unwrap();
    assert!(qos_ntfs_helper::protocol::identity(&report, 64).is_err());
}

#[test]
fn native_restart_valid_pair_is_clean() {
    assert_eq!(probe("restart-clean").unwrap()["status"], 0);
}
#[test]
fn native_restart_corrupt_first_refuses_without_clean_fact() {
    restart_refuses("restart-bad-first").unwrap();
}
#[test]
fn native_restart_corrupt_second_refuses_without_clean_fact() {
    restart_refuses("restart-bad-second").unwrap();
}
#[test]
fn native_restart_version_two_refuses_without_clean_fact() {
    restart_refuses("restart-v2").unwrap();
}
fn restart_refuses(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let report = probe(name)?;
    if report.get("status") != Some(&serde_json::json!(3)) {
        return Err(format!("{name}: expected refusal 3, got {:?}", report.get("status")).into());
    }
    let facts = report
        .get("facts")
        .and_then(serde_json::Value::as_array)
        .ok_or("missing facts")?;
    if facts.iter().any(|f| {
        f.get("kind") == Some(&serde_json::json!("logfile"))
            && f.get("state") == Some(&serde_json::json!("clean"))
    }) {
        return Err(format!("{name}: refused restart pair emitted clean logfile fact").into());
    }
    Ok(())
}
