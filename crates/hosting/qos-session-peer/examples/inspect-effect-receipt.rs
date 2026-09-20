//! Offline inspection of a FROZEN DISPOSABLE signed snapshot only.
//!
//! Usage: inspect-effect-receipt SNAPSHOT NEW_DESTINATION IDENTITY_ROOT HOST_IDENTITY_ROOT ORIGINAL_UUID
//! All paths are explicit; identity paths embedded in the snapshot are unused.
//! Import acquires disposable identity custody and writes only a new destination.
//! No service, action execution, seed, export or original-owner open is performed.
//! After this process exits, the operator may remove only the inspection destination.
//! Record source/identity hashes externally before and after inspection. This tool
//! proves a strict canonical receipt count, not effect-once or UI convergence.
use serde::Serialize;
use std::path::Path;
use tilo_actions::InvocationId;
use ui::automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value};

#[derive(Debug, Serialize)]
struct ReceiptProjection {
    matching_receipts: usize,
    key_matches_payload: bool,
    conflicts: usize,
    malformed_entries: usize,
    status: &'static str,
}

fn project(doc: &AutoCommit, original: InvocationId) -> ReceiptProjection {
    let mut result = ReceiptProjection {
        matching_receipts: 0,
        key_matches_payload: true,
        conflicts: 0,
        malformed_entries: 0,
        status: "missing",
    };
    let Ok(roots) = doc.get_all(ROOT, "effect_receipts_v1") else {
        result.malformed_entries = 1;
        result.status = "invalid";
        return result;
    };
    result.conflicts += roots.len().saturating_sub(1);
    let mut matching_status = None;
    for (value, object) in roots {
        if value != Value::Object(ObjType::Map) {
            result.malformed_entries += 1;
            continue;
        }
        for key in doc.keys(&object) {
            let Ok(values) = doc.get_all(&object, key.as_str()) else {
                result.malformed_entries += 1;
                continue;
            };
            result.conflicts += values.len().saturating_sub(1);
            if values.is_empty() {
                result.malformed_entries += 1;
            }
            for (value, _) in values {
                let receipt = match value {
                    Value::Scalar(value) => match value.as_ref() {
                        ScalarValue::Bytes(bytes) => {
                            serde_json::from_slice::<qos_desktop::WorkspaceEffectReceipt>(bytes)
                                .ok()
                        }
                        _ => None,
                    },
                    _ => None,
                };
                let Some(receipt) = receipt else {
                    result.malformed_entries += 1;
                    continue;
                };
                if key != receipt.invocation.0.to_string() {
                    result.key_matches_payload = false;
                    result.malformed_entries += 1;
                }
                if receipt.invocation == original {
                    result.matching_receipts += 1;
                    matching_status = Some(match receipt.status {
                        tilo_actions::EffectReceiptStatus::Succeeded => "succeeded",
                        tilo_actions::EffectReceiptStatus::Failed { .. } => "failed",
                        tilo_actions::EffectReceiptStatus::Cancelled => "cancelled",
                    });
                }
            }
        }
    }
    result.status =
        if result.conflicts != 0 || result.malformed_entries != 0 || result.matching_receipts > 1 {
            "invalid"
        } else {
            matching_status.unwrap_or("missing")
        };
    result
}

#[derive(Serialize)]
struct Evidence {
    room: String,
    generation: u64,
    room_heads: Vec<String>,
    original_invocation: String,
    #[serde(flatten)]
    receipt: ReceiptProjection,
}

fn inspect(
    snapshot: &Path,
    destination: &Path,
    identity_root: &Path,
    host_identity_root: &Path,
    original: InvocationId,
) -> Result<Evidence, &'static str> {
    match std::fs::symlink_metadata(destination) {
        Ok(_) => return Err("destination_exists"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => return Err("destination_unavailable"),
    }
    // Import verifies the signed aggregate and matching explicitly supplied
    // identities before exclusive destination creation. No embedded path fallback.
    let owner = qos_session_peer::SessionOwner::import(
        destination,
        snapshot,
        identity_root,
        host_identity_root,
    )
    .map_err(|_| "import_failed")?;
    let doc = owner.peer().admitted_room().ok_or("room_missing")?;
    let room = uuid::Uuid::parse_str(owner.peer().room())
        .map_err(|_| "invalid_room")?
        .to_string();
    let evidence = Evidence {
        room,
        generation: owner.status().generation,
        room_heads: doc
            .clone()
            .get_heads()
            .iter()
            .map(ToString::to_string)
            .collect(),
        original_invocation: original.0.to_string(),
        receipt: project(doc, original),
    };
    drop(owner);
    Ok(evidence)
}

fn run(args: Vec<std::ffi::OsString>) -> Result<Evidence, &'static str> {
    let [
        snapshot,
        destination,
        identity_root,
        host_identity_root,
        original,
    ] = args.as_slice()
    else {
        return Err("invalid_arguments");
    };
    let original = original
        .to_str()
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .ok_or("invalid_arguments")?;
    inspect(
        Path::new(snapshot),
        Path::new(destination),
        Path::new(identity_root),
        Path::new(host_identity_root),
        InvocationId(original),
    )
}

fn main() -> std::process::ExitCode {
    let (output, valid) = match run(std::env::args_os().skip(1).collect()) {
        Ok(evidence) => {
            let valid = evidence.receipt.matching_receipts == 1
                && evidence.receipt.key_matches_payload
                && evidence.receipt.conflicts == 0
                && evidence.receipt.malformed_entries == 0;
            (serde_json::to_string(&evidence), valid)
        }
        Err(status) => (
            serde_json::to_string(&serde_json::json!({"status": status})),
            false,
        ),
    };
    match output {
        Ok(output) => println!("{output}"),
        Err(_) => {
            println!("{{\"status\":\"encoding_failed\"}}");
            return std::process::ExitCode::FAILURE;
        }
    }
    if valid {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_desktop::WorkspaceEffectReceipt;
    use tilo_actions::{ActionContext, EffectReceiptStatus};
    use ui::automerge::{ObjType, ROOT, transaction::Transactable};
    use uuid::Uuid;

    fn original() -> InvocationId {
        InvocationId(Uuid::from_u128(11))
    }
    fn receipt(invocation: InvocationId) -> Vec<u8> {
        serde_json::to_vec(&WorkspaceEffectReceipt {
            invocation,
            actor: "private fixture actor".into(),
            context: ActionContext {
                action_label: "private fixture context".into(),
                ..Default::default()
            },
            status: EffectReceiptStatus::Succeeded,
            completed_at: 1,
        })
        .unwrap()
    }
    fn fixture() -> (AutoCommit, ui::automerge::ObjId) {
        let mut doc = AutoCommit::new();
        let map = doc
            .put_object(ROOT, "effect_receipts_v1", ObjType::Map)
            .unwrap();
        (doc, map)
    }
    #[test]
    fn exact_original_receipt_is_counted_without_private_projection() {
        let (mut doc, map) = fixture();
        doc.put(&map, original().0.to_string(), receipt(original()))
            .unwrap();
        let other = InvocationId(Uuid::from_u128(12));
        doc.put(&map, other.0.to_string(), receipt(other)).unwrap();
        let projection = project(&doc, original());
        assert_eq!(projection.matching_receipts, 1);
        assert_eq!(projection.status, "succeeded");
        assert!(projection.key_matches_payload);
        assert_eq!(projection.conflicts, 0);
        assert_eq!(projection.malformed_entries, 0);
        let output = serde_json::to_string(&projection).unwrap();
        assert!(!output.contains("private fixture"));
        assert!(!output.contains(&other.0.to_string()));
    }
    #[test]
    fn missing_original_receipt_is_not_success() {
        let (doc, _) = fixture();
        assert_eq!(project(&doc, original()).status, "missing");
    }
    #[test]
    fn wrong_key_rejects_embedded_original_receipt() {
        let (mut doc, map) = fixture();
        doc.put(&map, Uuid::from_u128(12).to_string(), receipt(original()))
            .unwrap();
        let projection = project(&doc, original());
        assert_eq!(projection.status, "invalid");
        assert!(!projection.key_matches_payload);
        assert_eq!(projection.malformed_entries, 1);
    }
    #[test]
    fn malformed_unrelated_entry_is_not_silently_skipped() {
        let (mut doc, map) = fixture();
        doc.put(&map, original().0.to_string(), receipt(original()))
            .unwrap();
        doc.put(&map, "unrelated", vec![0u8, 1]).unwrap();
        let projection = project(&doc, original());
        assert_eq!(projection.status, "invalid");
        assert_eq!(projection.malformed_entries, 1);
    }
    #[test]
    fn conflicting_receipt_values_are_rejected() {
        let (mut doc, map) = fixture();
        let mut other = doc.fork();
        doc.put(&map, original().0.to_string(), receipt(original()))
            .unwrap();
        let mut different: WorkspaceEffectReceipt =
            serde_json::from_slice(&receipt(original())).unwrap();
        different.completed_at = 2;
        other
            .put(
                &map,
                original().0.to_string(),
                serde_json::to_vec(&different).unwrap(),
            )
            .unwrap();
        doc.merge(&mut other).unwrap();
        let projection = project(&doc, original());
        assert_eq!(projection.status, "invalid");
        assert_eq!(projection.conflicts, 1);
    }
    #[test]
    fn conflicting_root_maps_are_rejected() {
        let mut doc = AutoCommit::new();
        let mut other = doc.fork();
        doc.put_object(ROOT, "effect_receipts_v1", ObjType::Map)
            .unwrap();
        other
            .put_object(ROOT, "effect_receipts_v1", ObjType::Map)
            .unwrap();
        doc.merge(&mut other).unwrap();
        assert_eq!(project(&doc, original()).conflicts, 1);
    }
    #[test]
    fn wrong_root_and_entry_types_are_rejected() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "effect_receipts_v1", "wrong type").unwrap();
        assert_eq!(project(&doc, original()).malformed_entries, 1);
        let (mut doc, map) = fixture();
        doc.put_object(&map, original().0.to_string(), ObjType::Map)
            .unwrap();
        assert_eq!(project(&doc, original()).malformed_entries, 1);
    }
    fn tree_digest(path: &Path) -> blake3::Hash {
        fn visit(path: &Path, hash: &mut blake3::Hasher) {
            let mut entries = std::fs::read_dir(path)
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                hash.update(entry.file_name().as_encoded_bytes());
                if entry.file_type().unwrap().is_dir() {
                    visit(&entry.path(), hash);
                } else {
                    hash.update(&std::fs::read(entry.path()).unwrap());
                }
            }
        }
        let mut hash = blake3::Hasher::new();
        visit(path, &mut hash);
        hash.finalize()
    }
    #[test]
    fn signed_disposable_import_preserves_source_and_explicit_identities() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let owner = qos_session_peer::SessionOwner::initialize(&source).unwrap();
        let expected = owner.status();
        drop(owner);
        let before = tree_digest(&source);
        let destination = temp.path().join("inspection");
        let evidence = inspect(
            &source.join("session.bin"),
            &destination,
            &source.join("identity"),
            &source.join("host-identity"),
            original(),
        )
        .unwrap();
        assert_eq!(evidence.room, expected.room);
        assert_eq!(evidence.generation, expected.generation);
        assert_eq!(evidence.receipt.status, "missing");
        assert!(!evidence.room_heads.is_empty());
        let output = serde_json::to_value(&evidence).unwrap();
        assert_eq!(
            output
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec![
                "conflicts",
                "generation",
                "key_matches_payload",
                "malformed_entries",
                "matching_receipts",
                "original_invocation",
                "room",
                "room_heads",
                "status"
            ]
        );
        assert!(!output.to_string().contains(source.to_str().unwrap()));
        assert!(destination.join("session.bin").is_file());
        assert_eq!(tree_digest(&source), before);
        // Inspection returned only after releasing custody; source is reopenable.
        drop(qos_session_peer::SessionOwner::open(&source).unwrap());
        std::fs::remove_dir_all(destination).unwrap();
    }
    #[test]
    fn existing_destination_is_rejected_before_import_without_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("occupied");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("sentinel"), b"keep").unwrap();
        let absent = temp.path().join("absent");
        assert_eq!(
            inspect(&absent, &destination, &absent, &absent, original()).err(),
            Some("destination_exists")
        );
        assert_eq!(
            std::fs::read(destination.join("sentinel")).unwrap(),
            b"keep"
        );
    }
    #[test]
    fn import_errors_are_categories_without_input_details() {
        let temp = tempfile::tempdir().unwrap();
        let snapshot = temp.path().join("private-source-name");
        std::fs::write(&snapshot, b"private malformed snapshot").unwrap();
        let absent = temp.path().join("private-identity-name");
        let destination = temp.path().join("inspection");
        assert_eq!(
            inspect(&snapshot, &destination, &absent, &absent, original()).err(),
            Some("import_failed")
        );
        assert!(!destination.exists());
        assert_eq!(
            std::fs::read(snapshot).unwrap(),
            b"private malformed snapshot"
        );
    }
}
