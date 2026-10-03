#![cfg(unix)]
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, process::Command};
fn fixtures() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(PathBuf::from(
        std::env::var_os("TILO_NTFS_FIXTURES")
            .ok_or("generate fixtures and set TILO_NTFS_FIXTURES")?,
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
    let output = Command::new(env!("CARGO_BIN_EXE_tilo-ntfs-image-test"))
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
/// Every fixture image probes to its expected status. `probe` itself fails
/// unless the complete image bytes are unchanged and no write was attempted,
/// on success and on failure alike.
#[test]
fn native_probe_classifies_each_image_without_mutating_it() {
    for (name, status) in [
        // Dirty log and volume flags refuse.
        ("dirty", 3),
        // Absent/zero hibernation file is clean; non-zero and short refuse.
        ("zero-hiber", 0),
        ("hiber", 4),
        ("nonzero-hiber", 4),
        ("short-hiber", 4),
        // A BitLocker signature is locked.
        ("bitlocker", 2),
        // Oversize is bounded.
        ("oversize", 6),
        // A valid restart pair is clean.
        ("restart-clean", 0),
    ] {
        assert_eq!(probe(name).unwrap()["status"], status, "case {name}");
    }
    // An invalid log, a hibernation-file I/O error and an attribute-list
    // cycle all refuse.
    for name in ["badlog", "hiber-io", "attrlist-cycle"] {
        assert_ne!(probe(name).unwrap()["status"], 0, "case {name}");
    }
    // A corrupt first or second restart page, or restart version two,
    // refuses without a clean logfile fact.
    for name in ["restart-bad-first", "restart-bad-second", "restart-v2"] {
        restart_refuses(name).unwrap();
    }
    // Complete image bytes are preserved on success and on failure (the
    // other images of this set are probed above).
    for name in ["clean", "truncated", "corrupt"] {
        let _ = probe(name).unwrap();
    }

    // A read-budget failure is explicit and non-mutating.
    let path = fixtures().unwrap().join("clean.img");
    let before = Sha256::digest(fs::read(&path).unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_tilo-ntfs-image-test"))
        .arg(&path)
        .args(["--read-limit", "512"])
        .output()
        .unwrap();
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["status"], 6);
    assert_eq!(before, Sha256::digest(fs::read(&path).unwrap()));
}

/// The clean, generation, upcase and ACL images report native identity,
/// content and security facts. Probing is read-only (see `probe`), so each
/// report is produced once and shared by the checks below.
#[test]
fn native_images_report_identity_content_and_security_facts() {
    let facts = |report: &serde_json::Value| report["facts"].as_array().expect("facts").clone();
    let clean = probe("clean").unwrap();
    let generation = probe("generation").unwrap();
    let upcase = probe("upcase").unwrap();

    // The clean image returns serial, root generation and content metadata.
    assert_eq!(clean["status"], 0, "{clean}");
    assert_eq!(clean["diagnostic_only"], true);
    let f = facts(&clean);
    assert!(
        f.iter()
            .any(|v| v["kind"] == "volume" && v["serial"] == "1122334455667788")
    );
    assert!(
        f.iter().any(|v| v["kind"] == "entry"
            && v["record"] == 5
            && v["sequence"].as_u64().unwrap() > 0)
    );
    assert!(
        f.iter().any(|v| v["kind"] == "chunk"
            && v["hex"] == "436f6e6e6563746564204e54465320666978747572650a")
    );

    // An MFT generation change changes identity.
    assert_eq!(generation["status"], 0, "{generation}");
    assert!(
        facts(&generation)
            .iter()
            .any(|v| v["kind"] == "entry" && v["record"] == 5 && v["sequence"] == 42)
    );

    // Upcase keys follow the volume table.
    assert_eq!(upcase["status"], 0, "{upcase}");
    assert!(facts(&upcase).iter().any(|v| v["kind"] == "entry"
        && v["name"] == "e4002e00740078007400"
        && v["key"] == "d6002e00540058005400"));

    // Identity binds the root sequence and the native upcase digest.
    assert!(
        clean["native_identity"]["volume"].is_string(),
        "missing native identity"
    );
    assert_eq!(
        clean["native_identity"]["volume"],
        generation["native_identity"]["volume"]
    );
    assert_ne!(
        clean["native_identity"]["root"],
        generation["native_identity"]["root"]
    );
    assert_ne!(
        clean["native_identity"]["upcase_sha256"],
        upcase["native_identity"]["upcase_sha256"]
    );

    // Security descriptors have native provenance.
    assert!(
        f.iter()
            .any(|v| v["kind"] == "security" && v["record"] == 5 && v["source"] == "attribute")
    );
    let acl = probe("acl").unwrap();
    assert!(
        facts(&acl)
            .iter()
            .any(|v| v["kind"] == "security" && v["record"] == 64 && v["source"] == "sds")
    );

    // A folder identity rejects a regular-file MFT locator.
    let report: tilo_ntfs_helper::protocol::Report = serde_json::from_value(clean).unwrap();
    assert!(tilo_ntfs_helper::protocol::identity(&report, 64).is_err());
}

/// Named directory streams, reparse points and hard-link resident attributes
/// are reported as distinct native facts.
#[test]
fn native_metadata_images_report_streams_reparse_and_hardlinks() {
    let facts = |report: &serde_json::Value| report["facts"].as_array().expect("facts").clone();
    let r = probe("metadata").unwrap();
    assert_eq!(r["status"], 0, "{r}");
    let f = facts(&r);
    assert!(f.iter().any(|v| v["kind"] == "attribute"
        && v["type"] == 128
        && v["record"] == 5
        && v["name"] == "640069007200"));
    assert!(
        f.iter()
            .any(|v| v["kind"] == "attribute" && v["type"] == 192)
    );

    let r = probe("hardlink").unwrap();
    assert_eq!(r["status"], 0, "{r}");
    assert!(facts(&r).iter().any(|v| {
        v["kind"] == "chunk"
            && v["attribute"] == 48
            && v["record"] == 64
            && v["hex"]
                .as_str()
                .is_some_and(|s| s.ends_with("7300650063006f006e006400"))
    }));
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
