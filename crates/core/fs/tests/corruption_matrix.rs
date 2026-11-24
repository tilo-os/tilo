//! Mandatory startup-corruption matrix for collaborative filesystem state.
//!
//! Every case distinguishes a missing store (which may intentionally start
//! empty) from a present but damaged store (which must fail startup without
//! rewriting the evidence or publishing a partial/default state).

use std::fs as stdfs;

use tempfile::tempdir;
use tilo_device_identity::IdentityPaths;
use tilo_fs::eviction::EvictionQueue;
use tilo_fs::{AvailabilityRegistry, Namespace};

fn truncated(bytes: &[u8]) -> Vec<u8> {
    let midpoint = bytes.len().div_ceil(2);
    bytes.get(..midpoint).unwrap_or(bytes).to_vec()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn corrupt_namespaces_stores_and_journals_are_preserved_and_fail_closed() {
    {
        let mut namespace = Namespace::new("corruption-source").unwrap();
        namespace
            .create_file(tilo_fs::ROOT_INO, "preserved.bin", None, 0, 1)
            .unwrap();
        let good = namespace.save();
        let mut control = Namespace::from_bytes("control", &good).unwrap();
        assert!(
            control
                .lookup(tilo_fs::ROOT_INO, b"preserved.bin")
                .is_some()
        );

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
                restored
                    .lookup(tilo_fs::ROOT_INO, b"preserved.bin")
                    .is_some(),
                "a caller-retained known-good snapshot remains a complete rollback boundary"
            );
        }
    }
    {
        let dir = tempdir().unwrap();
        let path = dir.path().join("namespace.bin");
        let mut first = Namespace::new("backup-source").unwrap();
        first
            .create_file(tilo_fs::ROOT_INO, "retained.txt", None, 0, 1)
            .unwrap();
        tilo_fs::durable_backup::write(&path, &first.save(), |bytes| {
            Namespace::from_bytes("validator", bytes).is_ok()
        })
        .unwrap();
        first
            .create_file(tilo_fs::ROOT_INO, "newer.txt", None, 0, 2)
            .unwrap();
        tilo_fs::durable_backup::write(&path, &first.save(), |bytes| {
            Namespace::from_bytes("validator", bytes).is_ok()
        })
        .unwrap();

        let damaged = b"not-an-automerge-document";
        stdfs::write(&path, damaged).unwrap();
        let outcome = tilo_fs::durable_backup::read_or_recover(&path, |bytes| {
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
        assert!(
            recovered
                .lookup(tilo_fs::ROOT_INO, b"retained.txt")
                .is_some()
        );
        assert!(
            recovered.lookup(tilo_fs::ROOT_INO, b"newer.txt").is_none(),
            "recovery chooses the last completed generation; signed-record reconciliation replays later state"
        );
    }
    {
        let dir = tempdir().unwrap();
        let damaged = b"{\"schema_version\":";
        let availability = dir.path().join("availability.json");
        let eviction = dir.path().join("evictions.json");

        for path in [&availability, &eviction] {
            stdfs::write(path, damaged).unwrap();
        }

        assert!(AvailabilityRegistry::open(&availability).is_err());
        assert!(EvictionQueue::open(&eviction).is_err());
        for path in [&availability, &eviction] {
            assert_eq!(
                stdfs::read(path).unwrap(),
                damaged,
                "startup validation must not overwrite corruption with an empty store"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn a_damaged_device_certificate_is_preserved_and_fails_closed() {
    use tilo_device_identity::persist::write_device_cert;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    let dir = tempdir().unwrap();
    let paths = IdentityPaths::under(dir.path());
    let root = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
    let device_public_key = device.verifying_key().to_bytes();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key,
        device_label: "fixture".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    write_device_cert(&paths.device_cert, &cert).unwrap();

    let good = stdfs::read(&paths.device_cert).unwrap();
    let damaged = truncated(&good);
    stdfs::write(&paths.device_cert, &damaged).unwrap();

    let now = tilo_clock::unix_now_ns();
    assert!(
        tilo_fs::load_certified_mount_identity(&paths, device_public_key, now).is_err(),
        "a present but damaged identity must fail startup"
    );
    assert_eq!(
        stdfs::read(&paths.device_cert).unwrap(),
        damaged,
        "identity startup must preserve damaged evidence rather than mint a new identity"
    );

    stdfs::write(&paths.device_cert, good).unwrap();
    let restored = tilo_fs::load_certified_mount_identity(&paths, device_public_key, now).unwrap();
    assert_eq!(restored.device_public_key(), device_public_key);
}
