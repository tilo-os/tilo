//! Mandatory startup-corruption matrix for collaborative filesystem state.
//!
//! Every case distinguishes a missing store (which may intentionally start
//! empty) from a present but damaged store (which must fail startup without
//! rewriting the evidence or publishing a partial/default state).

use std::fs as stdfs;

use collab_types::FileRecordId;
use fs::eviction::EvictionQueue;
use fs::{
    AvailabilityRegistry, FileHistoryStore, Namespace, RecordRepository, RecordTransactionJournal,
    load_mount_identity,
};
use qos_user_identity::IdentityPaths;
use tempfile::tempdir;
use uuid::Uuid;

fn truncated(bytes: &[u8]) -> Vec<u8> {
    let midpoint = bytes.len().div_ceil(2);
    bytes.get(..midpoint).unwrap_or(bytes).to_vec()
}

#[test]
fn namespace_corruption_never_becomes_an_empty_workspace() {
    let mut namespace = Namespace::new("corruption-source").unwrap();
    namespace
        .create_file(fs::ROOT_INO, "preserved.bin", None, 0, 1)
        .unwrap();
    let good = namespace.save();
    let mut control = Namespace::from_bytes("control", &good).unwrap();
    assert!(control.lookup(fs::ROOT_INO, b"preserved.bin").is_some());

    for damaged in [truncated(&good), {
        let mut value = good.clone();
        let midpoint = value.len() / 2;
        value[midpoint] ^= 0xff;
        value
    }] {
        assert!(
            Namespace::from_bytes("damaged", &damaged).is_err(),
            "present corrupted namespace bytes must not silently create an empty workspace"
        );
        let mut restored = Namespace::from_bytes("restored", &good).unwrap();
        assert!(
            restored.lookup(fs::ROOT_INO, b"preserved.bin").is_some(),
            "a caller-retained known-good snapshot remains a complete rollback boundary"
        );
    }
}

#[test]
fn namespace_automatic_recovery_is_bounded_explicit_and_preserves_damage() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("namespace.bin");
    let mut first = Namespace::new("backup-source").unwrap();
    first
        .create_file(fs::ROOT_INO, "retained.txt", None, 0, 1)
        .unwrap();
    fs::durable_backup::write(&path, &first.save(), |bytes| {
        Namespace::from_bytes("validator", bytes).is_ok()
    })
    .unwrap();
    first
        .create_file(fs::ROOT_INO, "newer.txt", None, 0, 2)
        .unwrap();
    fs::durable_backup::write(&path, &first.save(), |bytes| {
        Namespace::from_bytes("validator", bytes).is_ok()
    })
    .unwrap();

    let damaged = b"not-an-automerge-document";
    stdfs::write(&path, damaged).unwrap();
    let outcome = fs::durable_backup::read_or_recover(&path, |bytes| {
        Namespace::from_bytes("validator", bytes).is_ok()
    })
    .unwrap()
    .unwrap();
    let evidence = outcome
        .recovery
        .expect("automatic rollback must report recovery evidence");
    assert_eq!(
        stdfs::read(&evidence.damaged_evidence_path).unwrap(),
        damaged
    );
    assert!(evidence.receipt_path.is_file());
    let mut recovered = Namespace::from_bytes("recovered", &outcome.bytes).unwrap();
    assert!(recovered.lookup(fs::ROOT_INO, b"retained.txt").is_some());
    assert!(
        recovered.lookup(fs::ROOT_INO, b"newer.txt").is_none(),
        "recovery chooses the last completed generation; signed-record reconciliation replays later state"
    );
}

#[test]
fn malformed_single_file_stores_are_preserved_and_fail_closed() {
    let dir = tempdir().unwrap();
    let damaged = b"{\"schema_version\":";
    let history = dir.path().join("history.json");
    let availability = dir.path().join("availability.json");
    let eviction = dir.path().join("evictions.json");

    for path in [&history, &availability, &eviction] {
        stdfs::write(path, damaged).unwrap();
    }

    assert!(FileHistoryStore::open(&history).is_err());
    assert!(AvailabilityRegistry::open(&availability).is_err());
    assert!(EvictionQueue::open(&eviction).is_err());
    for path in [&history, &availability, &eviction] {
        assert_eq!(
            stdfs::read(path).unwrap(),
            damaged,
            "startup validation must not overwrite corruption with an empty store"
        );
    }
}

#[test]
fn malformed_repository_and_transaction_journal_publish_nothing() {
    let dir = tempdir().unwrap();
    let repository_root = dir.path().join("records");
    let workspace = Uuid::new_v4();
    let record = FileRecordId::new();
    let record_dir = repository_root.join(workspace.to_string());
    stdfs::create_dir_all(&record_dir).unwrap();
    let record_path = record_dir.join(format!("{}.json", record.0));
    stdfs::write(&record_path, b"{\"schema_version\":1").unwrap();

    assert!(RecordRepository::open(&repository_root).is_err());
    assert_eq!(stdfs::read(&record_path).unwrap(), b"{\"schema_version\":1");

    let journal_root = dir.path().join("transactions");
    stdfs::create_dir_all(&journal_root).unwrap();
    let transaction_path = journal_root.join(format!("{}.json", Uuid::new_v4()));
    stdfs::write(&transaction_path, b"[]").unwrap();
    assert!(RecordTransactionJournal::open(&journal_root).is_err());
    assert_eq!(stdfs::read(&transaction_path).unwrap(), b"[]");

    assert!(
        !dir.path().join("namespace.bin").exists(),
        "a coordinator cannot project namespace state when its journal or repository refuses open"
    );
}

#[cfg(unix)]
#[test]
fn truncated_device_identity_is_not_replaced_with_a_new_actor() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let paths = IdentityPaths::under(dir.path());
    let first = load_mount_identity(&paths).unwrap();
    let original_actor = first.public;
    drop(first);

    let good = stdfs::read(&paths.device_privkey).unwrap();
    let damaged = truncated(&good);
    stdfs::write(&paths.device_privkey, &damaged).unwrap();
    stdfs::set_permissions(&paths.device_privkey, stdfs::Permissions::from_mode(0o600)).unwrap();

    assert!(load_mount_identity(&paths).is_err());
    assert_eq!(
        stdfs::read(&paths.device_privkey).unwrap(),
        damaged,
        "identity startup must preserve damaged evidence rather than rotate identity"
    );

    stdfs::write(&paths.device_privkey, good).unwrap();
    let restored = load_mount_identity(&paths).unwrap();
    assert_eq!(
        restored.public, original_actor,
        "restoring a known-good key is an exact actor rollback"
    );
}
