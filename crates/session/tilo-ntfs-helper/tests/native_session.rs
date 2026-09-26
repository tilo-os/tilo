#![cfg(unix)]
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, process::Command};
type TestResult<T> = Result<T, Box<dyn std::error::Error>>;
fn image(name: &str) -> TestResult<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("TILO_NTFS_FIXTURES").ok_or("native fixture path")?)
            .join(format!("{name}.img")),
    )
}
fn operation(name: &str, args: &[String]) -> TestResult<Value> {
    let path = image(name)?;
    let before = Sha256::digest(fs::read(&path)?);
    let output = Command::new(env!("CARGO_BIN_EXE_tilo-ntfs-image-test"))
        .arg(&path)
        .args(args)
        .output()?;
    if before != Sha256::digest(fs::read(&path)?) {
        return Err("complete image changed".into());
    }
    if !output.status.success() {
        return Err(format!("{output:?}").into());
    }
    let report: Value = serde_json::from_slice(&output.stdout)?;
    if report.pointer("/stats/writes").and_then(Value::as_u64) != Some(0) {
        return Err("native write attempt".into());
    }
    Ok(report)
}
fn manifest(name: &str) -> TestResult<Value> {
    operation(name, &["--manifest".into()])
}
fn request(m: &Value, name: &str) -> TestResult<Value> {
    let encoded = hex::encode(
        name.encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<u8>>(),
    );
    let facts = m.get("facts").and_then(Value::as_array).ok_or("facts")?;
    let entry = facts
        .iter()
        .find(|f| {
            f.get("kind").and_then(Value::as_str) == Some("entry")
                && f.get("name").and_then(Value::as_str) == Some(&encoded)
        })
        .ok_or("entry")?;
    let record = entry.get("record").ok_or("record")?;
    let attr = facts
        .iter()
        .find(|f| {
            f.get("kind").and_then(Value::as_str) == Some("attribute")
                && f.get("record") == Some(record)
                && f.get("type").and_then(Value::as_u64) == Some(128)
                && f.get("name").and_then(Value::as_str) == Some("")
        })
        .ok_or("data")?;
    Ok(
        json!({"serial":0x1122_3344_5566_7788_u64,"root_record":5,"root_sequence":m.pointer("/native_identity/root_sequence").ok_or("root sequence")?,"root_creation":m.pointer("/native_identity/root_creation").ok_or("creation")?,"object_record":record,"object_sequence":entry.get("sequence").ok_or("sequence")?,"size":attr.get("size").ok_or("size")?,"offset":0,"length":1_048_576}),
    )
}
fn bytes(r: &Value) -> TestResult<Vec<u8>> {
    let mut result = Vec::new();
    for f in r
        .get("facts")
        .and_then(Value::as_array)
        .ok_or("facts")?
        .iter()
        .filter(|f| f.get("kind").and_then(Value::as_str) == Some("data"))
    {
        if f.get("offset").and_then(Value::as_u64) != Some(u64::try_from(result.len())?) {
            return Err("coverage".into());
        }
        for pair in f
            .get("hex")
            .and_then(Value::as_str)
            .ok_or("hex")?
            .as_bytes()
            .chunks_exact(2)
        {
            result.push(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?);
        }
    }
    Ok(result)
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
)]
fn native_manifest_scopes_metadata_and_completions_over_the_real_image() {
    // Phase `metadata_manifest_and_twelve_native_chunks_preserve_real_image`.
    {
        let m = manifest("large").unwrap();
        assert_eq!(m["status"], 0);
        assert_eq!(m["schema"], 2);
        let mut req = request(&m, "large.bin").unwrap();
        let record = req["object_record"].clone();
        assert!(
            !m["facts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["kind"] == "chunk" && f["record"] == record && f["attribute"] == 128)
        );
        let mut hash = Sha256::new();
        let mut chunk_hashes = std::collections::BTreeSet::new();
        for n in 0..12 {
            req["offset"] = json!(n * 1_048_576_u64);
            let r = operation("large", &["--read-chunk".into(), req.to_string()]).unwrap();
            assert_eq!(r["status"], 0);
            let chunk = bytes(&r).unwrap();
            assert_eq!(chunk.len(), 1_048_576);
            assert!(chunk_hashes.insert(hex::encode(Sha256::digest(&chunk))));
            hash.update(chunk);
        }
        let expected: Vec<u8> = (0..12 * 1024 * 1024)
            .map(|n| u8::try_from((n % 256 + n / 1_048_576) % 256).unwrap())
            .collect();
        assert_eq!(hash.finalize(), Sha256::digest(expected));
        req["offset"] = json!(1_048_573);
        req["length"] = json!(9);
        let crossing = operation("large", &["--read-chunk".into(), req.to_string()]).unwrap();
        assert_eq!(
            bytes(&crossing).unwrap(),
            vec![253, 254, 255, 1, 2, 3, 4, 5, 6]
        );
        let mut tail = request(&m, "tail.bin").unwrap();
        tail["offset"] = json!(3);
        tail["length"] = json!(4264);
        let r = operation("large", &["--read-chunk".into(), tail.to_string()]).unwrap();
        assert_eq!(r["status"], 0);
        assert_eq!(
            bytes(&r).unwrap(),
            (0..4267)
                .map(|n| u8::try_from(n % 251).unwrap())
                .skip(3)
                .collect::<Vec<_>>()
        );
        assert_eq!(request(&m, "empty.bin").unwrap()["size"], 0);
    }
    // Phase `positive_attribute_and_directory_completion_is_emitted`.
    {
        let m = manifest("clean").unwrap();
        assert_eq!(m["status"], 0);
        let facts = m["facts"].as_array().unwrap();
        for entry in facts.iter().filter(|f| f["kind"] == "entry") {
            assert!(facts.iter().any(|f| f["kind"] == "attributes_end"
                && f["record"] == entry["record"]
                && f["sequence"] == entry["sequence"]));
            if entry["directory"] == true {
                assert!(facts.iter().any(|f| f["kind"] == "directory_end"
                    && f["record"] == entry["record"]
                    && f["sequence"] == entry["sequence"]));
            }
        }
    }
    // Phase `typed_manifest_requires_positive_completions_and_scopes_native_metadata`.
    {
        let raw = manifest("metadata").unwrap();
        let report: tilo_ntfs_helper::protocol::Report = serde_json::from_value(raw).unwrap();
        let checked = tilo_ntfs_helper::manifest::verify(&report, 5).unwrap();
        assert!(checked.entries.iter().any(|e| e.content_readable
            && e.path == vec!["hello.txt".encode_utf16().collect::<Vec<_>>()]));
        assert!(checked.issues.iter().any(|i| i.path.is_none()));
        let mut missing = report;
        missing.facts.retain(|f| f["kind"] != "attributes_end");
        assert!(tilo_ntfs_helper::manifest::verify(&missing, 5).is_err());
    }
    // Phase `manifest_missing_directory_completion_is_not_positive_enumeration`.
    {
        let raw = manifest("clean").unwrap();
        let mut report: tilo_ntfs_helper::protocol::Report = serde_json::from_value(raw).unwrap();
        report.facts.retain(|f| f["kind"] != "directory_end");
        assert!(tilo_ntfs_helper::manifest::verify(&report, 5).is_err());
    }
    // Phase `manifest_oversize_metadata_is_scoped_and_content_remains_readable`.
    {
        let raw = manifest("large-metadata").unwrap();
        assert_eq!(raw["status"], 0);
        let report: tilo_ntfs_helper::protocol::Report =
            serde_json::from_value(raw.clone()).unwrap();
        let checked = tilo_ntfs_helper::manifest::verify(&report, 5).unwrap();
        let hello = vec!["hello.txt".encode_utf16().collect::<Vec<_>>()];
        assert!(
            checked
                .issues
                .iter()
                .any(|i| i.path == Some(hello.clone()) && i.reason == "metadata-budget")
        );
        let entry = checked.entries.iter().find(|e| e.path == hello).unwrap();
        assert!(entry.content_readable);
        let mut req = request(&raw, "hello.txt").unwrap();
        req["length"] = json!(23);
        let read = operation("large-metadata", &["--read-chunk".into(), req.to_string()]).unwrap();
        assert_eq!(read["status"], 0);
        assert_eq!(bytes(&read).unwrap(), b"Connected NTFS fixture\n");
    }
    // Phase `copy_manifest_diagnostic_exposes_verified_portable_entries`.
    {
        let report = operation("large", &["--copy-manifest".into()]).unwrap();
        let entries = report.get("entries").and_then(Value::as_array).unwrap();
        let large = entries
            .iter()
            .find(|e| e["size"] == 12 * 1_048_576)
            .unwrap();
        assert!(large["copy_times"].is_object());
        assert_eq!(large["content_readable"], true);
        assert_eq!(report["diagnostic_only"], true);
    }
    // Phase `native_manifest_resource_failure_preserves_image`.
    {
        let r = operation(
            "large",
            &["--manifest".into(), "--read-limit".into(), "512".into()],
        )
        .unwrap();
        assert_ne!(r["status"], 0);
        assert!(r["native_identity"].is_null());
    }
}
#[test]
fn native_ranges_agree_with_metadata_and_refuse_stale_or_unverified_reads() {
    // Phase `native_ranges_and_changed_generation_refuse`.
    {
        let m = manifest("large").unwrap();
        let base = request(&m, "large.bin").unwrap();
        for (key, value) in [
            ("offset", u64::MAX),
            ("offset", 12 * 1_048_576),
            ("length", 1_048_577),
            ("length", 0),
            ("serial", 1),
            ("object_sequence", 42),
            ("root_sequence", 42),
            ("root_creation", 1),
            ("size", 42),
        ] {
            let mut req = base.clone();
            req[key] = json!(value);
            let r = operation("large", &["--read-chunk".into(), req.to_string()]).unwrap();
            assert_ne!(r["status"], 0, "{key}");
        }
        let old = manifest("clean").unwrap();
        let mut req = request(&old, "hello.txt").unwrap();
        req["length"] = json!(22);
        assert_ne!(
            operation("generation", &["--read-chunk".into(), req.to_string()]).unwrap()["status"],
            0
        );
    }
    // Phase `native_range_metadata_agrees_before_after_and_with_manifest`.
    {
        for image in ["clean", "hardlink"] {
            let raw = manifest(image).unwrap();
            let report: tilo_ntfs_helper::protocol::Report =
                serde_json::from_value(raw.clone()).unwrap();
            let checked = tilo_ntfs_helper::manifest::verify(&report, 5).unwrap();
            let entry = checked
                .entries
                .iter()
                .find(|e| e.path == vec!["hello.txt".encode_utf16().collect::<Vec<_>>()])
                .unwrap();
            let mut req = request(&raw, "hello.txt").unwrap();
            req["length"] = json!(23);
            let raw = operation(image, &["--read-chunk".into(), req.to_string()]).unwrap();
            let chunk: tilo_ntfs_helper::protocol::Report = serde_json::from_value(raw).unwrap();
            assert_eq!(
                chunk
                    .facts
                    .iter()
                    .filter(|f| f["kind"] == "attributes_end")
                    .count(),
                2
            );
            assert_eq!(
                tilo_ntfs_helper::manifest::metadata_digest(&chunk.facts, entry.record).unwrap(),
                entry.metadata_digest
            );
        }
    }
    // Phase `native_chunk_missing_postmetadata_cannot_publish_successful_bytes`.
    {
        let m = manifest("clean").unwrap();
        let mut req = request(&m, "hello.txt").unwrap();
        req["length"] = json!(23);
        let raw = operation("clean", &["--read-chunk".into(), req.to_string()]).unwrap();
        let mut report: tilo_ntfs_helper::protocol::Report = serde_json::from_value(raw).unwrap();
        let last = report
            .facts
            .iter()
            .rposition(|f| f["kind"] == "attributes_end")
            .unwrap();
        report.facts.remove(last);
        assert!(tilo_ntfs_helper::limits::decode_chunk(&report, 23).is_err());
    }
    // Phase `cancellation_fixture_has_no_entry_after_its_last_read`.
    {
        let m = manifest("large").unwrap();
        let facts = m["facts"].as_array().unwrap();
        let directory = facts
            .iter()
            .find(|f| f["kind"] == "entry" && f["name"] == "630061006e00630065006c00")
            .expect("native cancellation subtree missing");
        assert_eq!(directory["directory"], true);
        let children: Vec<_> = facts
            .iter()
            .filter(|f| f["kind"] == "entry" && f["parent"] == directory["record"])
            .collect();
        assert_eq!(
            children.len(),
            1,
            "last-chunk cancellation needs a final readable entry"
        );
        assert_eq!(children[0]["name"], "6c006100730074002e00620069006e00");
        let mut req = request(&m, "last.bin").unwrap();
        req["length"] = json!(11);
        let r = operation("large", &["--read-chunk".into(), req.to_string()]).unwrap();
        assert_eq!(bytes(&r).unwrap(), b"final chunk");
    }
}
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
)]
fn standard_information_times_are_exact_or_typed_unavailable() {
    // Phase `native_standard_information_times_preserve_exact_signed_100ns_values`.
    {
        for image in ["times", "times-v1"] {
            let raw = manifest(image).unwrap();
            let expected_layout = if image == "times" { 72 } else { 48 };
            let facts = raw["facts"].as_array().unwrap();
            let record = facts
                .iter()
                .find(|fact| {
                    fact["kind"] == "entry"
                        && fact["name"] == "680065006c006c006f002e00740078007400"
                })
                .unwrap()["record"]
                .clone();
            assert!(facts.iter().any(|fact| {
                fact["kind"] == "attribute"
                    && fact["record"] == record
                    && fact["type"] == 16
                    && fact["name"] == ""
                    && fact["size"] == expected_layout
            }));
            let report: tilo_ntfs_helper::protocol::Report = serde_json::from_value(raw).unwrap();
            let checked = tilo_ntfs_helper::manifest::verify(&report, 5).unwrap();
            let hello = checked
                .entries
                .iter()
                .find(|entry| entry.path == vec!["hello.txt".encode_utf16().collect::<Vec<_>>()])
                .unwrap();
            assert_eq!(
                hello.copy_times,
                Some(tilo_ntfs_protocol::CopyTimes {
                    last_access_100ns: 116_444_736_012_345_678,
                    last_data_change_100ns: 116_444_735_999_999_999,
                }),
                "{image}"
            );
            assert_eq!(hello.copy_times_unavailable_reason, None, "{image}");
        }
    }
    // Phase `native_negative_standard_information_images_cross_parser_boundary`.
    {
        use tilo_ntfs_protocol::CopyTimesUnavailableReason as Reason;

        let missing = manifest("times-missing").unwrap();
        assert_eq!(
            missing["status"], 5,
            "missing standard information must fail closed"
        );
        assert_eq!(missing["metadata_complete"], false);
        assert!(missing["native_identity"].is_null());

        for (image, reason, declarations, flags) in [
            (
                "times-duplicate",
                Reason::DuplicateStandardInformation,
                2,
                0,
            ),
            (
                "times-invalid-flags",
                Reason::InvalidStandardInformationCoverage,
                1,
                1,
            ),
        ] {
            let raw = manifest(image).unwrap();
            assert_eq!(raw["status"], 0, "{image}");
            let facts = raw["facts"].as_array().unwrap();
            let record = facts
                .iter()
                .find(|fact| {
                    fact["kind"] == "entry"
                        && fact["name"] == "680065006c006c006f002e00740078007400"
                })
                .unwrap()["record"]
                .clone();
            let standard_information: Vec<_> = facts
                .iter()
                .filter(|fact| {
                    fact["kind"] == "attribute"
                        && fact["record"] == record
                        && fact["type"] == 16
                        && fact["name"] == ""
                })
                .collect();
            assert_eq!(standard_information.len(), declarations, "{image}");
            assert!(
                standard_information
                    .iter()
                    .all(|fact| fact["flags"] == flags)
            );
            let report: tilo_ntfs_helper::protocol::Report = serde_json::from_value(raw).unwrap();
            let checked = tilo_ntfs_helper::manifest::verify(&report, 5).unwrap();
            let entry = checked
                .entries
                .iter()
                .find(|entry| entry.path == vec!["hello.txt".encode_utf16().collect::<Vec<_>>()])
                .unwrap();
            assert!(entry.content_readable, "{image}");
            assert_eq!(entry.copy_times, None, "{image}");
            assert_eq!(entry.copy_times_unavailable_reason, Some(reason), "{image}");
        }
    }
    // Phase `copy_times_unavailability_is_typed_and_does_not_hide_readable_content`.
    {
        use tilo_ntfs_protocol::CopyTimesUnavailableReason as Reason;

        let raw = manifest("times").unwrap();
        let report: tilo_ntfs_helper::protocol::Report = serde_json::from_value(raw).unwrap();
        let hello_record = report
            .facts
            .iter()
            .find(|fact| {
                fact["kind"] == "entry" && fact["name"] == "680065006c006c006f002e00740078007400"
            })
            .and_then(|fact| fact["record"].as_u64())
            .unwrap();
        let check = |report: tilo_ntfs_helper::protocol::Report, reason| {
            let checked = tilo_ntfs_helper::manifest::verify(&report, 5).unwrap();
            let entry = checked
                .entries
                .iter()
                .find(|entry| entry.record == hello_record)
                .unwrap();
            assert!(entry.content_readable);
            assert_eq!(entry.copy_times, None);
            assert_eq!(entry.copy_times_unavailable_reason, Some(reason));
        };

        let mut missing: tilo_ntfs_helper::protocol::Report =
            serde_json::from_value(manifest("times").unwrap()).unwrap();
        missing.facts.retain(|fact| {
            fact["record"] != hello_record
                || !((fact["kind"] == "attribute" && fact["type"] == 16)
                    || (fact["kind"] == "chunk" && fact["attribute"] == 16))
        });
        check(missing, Reason::MissingStandardInformation);

        let mut duplicate: tilo_ntfs_helper::protocol::Report =
            serde_json::from_value(manifest("times").unwrap()).unwrap();
        let at = duplicate
            .facts
            .iter()
            .position(|fact| {
                fact["record"] == hello_record && fact["kind"] == "attribute" && fact["type"] == 16
            })
            .unwrap();
        let extra = duplicate.facts[at].clone();
        duplicate.facts.insert(at, extra);
        check(duplicate, Reason::DuplicateStandardInformation);

        let mut unsupported: tilo_ntfs_helper::protocol::Report =
            serde_json::from_value(manifest("times").unwrap()).unwrap();
        unsupported
            .facts
            .iter_mut()
            .find(|fact| {
                fact["record"] == hello_record && fact["kind"] == "attribute" && fact["type"] == 16
            })
            .unwrap()["size"] = json!(64);
        check(unsupported, Reason::UnsupportedStandardInformationLayout);

        let mut invalid: tilo_ntfs_helper::protocol::Report =
            serde_json::from_value(manifest("times").unwrap()).unwrap();
        invalid
            .facts
            .iter_mut()
            .find(|fact| {
                fact["record"] == hello_record && fact["kind"] == "chunk" && fact["attribute"] == 16
            })
            .unwrap()["offset"] = json!(1);
        check(invalid, Reason::InvalidStandardInformationCoverage);

        let mut wrong_instance: tilo_ntfs_helper::protocol::Report =
            serde_json::from_value(manifest("times").unwrap()).unwrap();
        wrong_instance
            .facts
            .iter_mut()
            .find(|fact| {
                fact["record"] == hello_record && fact["kind"] == "chunk" && fact["attribute"] == 16
            })
            .unwrap()["instance"] = json!(65_535);
        check(wrong_instance, Reason::InvalidStandardInformationCoverage);

        let mut invalid_flags: tilo_ntfs_helper::protocol::Report =
            serde_json::from_value(manifest("times").unwrap()).unwrap();
        invalid_flags
            .facts
            .iter_mut()
            .find(|fact| {
                fact["record"] == hello_record && fact["kind"] == "attribute" && fact["type"] == 16
            })
            .unwrap()["flags"] = json!(1);
        check(invalid_flags, Reason::InvalidStandardInformationCoverage);
    }
}

#[test]
fn native_decoders_reject_malformed_payloads_and_keep_aliases_explicit() {
    // Phase `native_metadata_and_read_payload_decoders_reject_duplicate_gap_overlap_and_trailer`.
    {
        let raw = manifest("clean").unwrap();
        for kind in ["attributes_end", "directory_end"] {
            let mut report: tilo_ntfs_helper::protocol::Report =
                serde_json::from_value(raw.clone()).unwrap();
            let index = report.facts.iter().position(|f| f["kind"] == kind).unwrap();
            let duplicate = report.facts[index].clone();
            report.facts.insert(index, duplicate);
            assert!(tilo_ntfs_helper::manifest::verify(&report, 5).is_err());
        }
        let mut req = request(&raw, "hello.txt").unwrap();
        req["length"] = json!(22);
        let raw = operation("clean", &["--read-chunk".into(), req.to_string()]).unwrap();
        for offset in [1, 21, u64::MAX] {
            let mut report: tilo_ntfs_helper::protocol::Report =
                serde_json::from_value(raw.clone()).unwrap();
            report
                .facts
                .iter_mut()
                .find(|f| f["kind"] == "data")
                .unwrap()["offset"] = json!(offset);
            assert!(tilo_ntfs_helper::limits::decode_chunk(&report, 22).is_err());
        }
        let mut report: tilo_ntfs_helper::protocol::Report = serde_json::from_value(raw).unwrap();
        let duplicate = report
            .facts
            .iter()
            .find(|f| f["kind"] == "data")
            .unwrap()
            .clone();
        report.facts.push(duplicate);
        assert!(tilo_ntfs_helper::limits::decode_chunk(&report, 22).is_err());
    }
    // Phase `typed_native_security_aliases_reparse_and_upcase_remain_explicit`.
    {
        for image in ["metadata", "acl", "hardlink", "upcase"] {
            let r = manifest(image).unwrap();
            let report: tilo_ntfs_helper::protocol::Report = serde_json::from_value(r).unwrap();
            let verified = tilo_ntfs_helper::manifest::verify(&report, 5).unwrap();
            assert!(
                verified
                    .entries
                    .iter()
                    .all(|e| !e.fidelity_pending.is_empty())
            );
            if image == "upcase" {
                assert_eq!(verified.upcase[0xe4], 0xd6);
            }
            if image == "hardlink" {
                assert!(
                    verified
                        .issues
                        .iter()
                        .any(|i| i.path.is_none() && i.reason == "external-alias-closure-unknown")
                );
            }
            if image == "metadata" {
                assert!(verified.entries.iter().any(|e| e.path
                    == vec!["link".encode_utf16().collect::<Vec<_>>()]
                    && !e.content_readable));
            }
        }
    }
    // Phase `native_dos_aliases_are_pending_aliases_not_independent_files`.
    {
        let raw = manifest("dos-alias").unwrap();
        assert_eq!(raw["status"], 0);
        assert!(
            raw["facts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["kind"] == "entry" && f["namespace"] == 2)
        );
        let report: tilo_ntfs_helper::protocol::Report = serde_json::from_value(raw).unwrap();
        let checked = tilo_ntfs_helper::manifest::verify(&report, 5).unwrap();
        assert!(checked.entries.iter().any(|e| e.path
            == vec!["hello.txt".encode_utf16().collect::<Vec<_>>()]
            && e.content_readable));
        assert!(
            !checked
                .entries
                .iter()
                .any(|e| e.path == vec!["HELLO~1.TXT".encode_utf16().collect::<Vec<_>>()])
        );
        assert!(
            checked
                .issues
                .iter()
                .any(|i| i.reason == "dos-alias-relationship-pending")
        );
    }
}
