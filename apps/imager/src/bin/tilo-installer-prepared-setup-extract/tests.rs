use super::{fs, payload};
use prepared_setup_model::{Home, HomeExpectation, Limits};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;

fn private(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}
fn expectation(data: &[u8]) -> Value {
    let operation = "example-home".to_owned();
    let expected = HomeExpectation {
        filename: HomeExpectation::filename(&operation).unwrap(),
        operation,
        setup_binding: "example setup".into(),
        sealed_bytes: data.len() as u64,
        plaintext_bytes: 10,
        entries: 1,
        ciphertext_sha256: Sha256::digest(data).into(),
        limits: Limits {
            plaintext_bytes: 1024,
            entries: 10,
            metadata_bytes: 1024,
        },
        kind: Home { runtime_sockets: 0 },
    };
    serde_json::to_value(&expected).unwrap()
}
fn flat_expectation(data: &[u8]) -> Value {
    serde_json::json!({
        "schema": 1,
        "operation": "example-home",
        "purpose": "home",
        "filename": "home-example-home.sealed",
        "setup_binding": "example setup",
        "sealed_bytes": data.len(),
        "plaintext_bytes": 10,
        "entries": 1,
        "runtime_sockets": 0,
        "ciphertext_sha256": Sha256::digest(data).to_vec(),
        "limits": {"plaintext_bytes": 1024, "entries": 10, "metadata_bytes": 1024}
    })
}
tilo_test_env::scenario! {
    fn the_production_handoff_retains_ciphertext_until_verified;

    phase production_hardlink_handoff_retains_until_verified_and_retries_without_archive;
    {
        let data = [b"TILOMIG1".as_slice(), &[42; 72]].concat();
        let canonical = expectation(&data);
        for e in [flat_expectation(&data), canonical.clone()] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            let home = temp.path().join("home");
            private(&source).unwrap();
            private(&home).unwrap();
            let source_fd = fs::directory(source.to_str().unwrap()).unwrap();
            fs::publish_json(&source_fd, "expectation.json", &e, fs::uid(), fs::gid()).unwrap();
            let temp_file = fs::Temporary::new(&source_fd, fs::uid(), fs::gid()).unwrap();
            std::io::Write::write_all(&mut &temp_file.fd, &data).unwrap();
            temp_file.publish("home-example-home.sealed").unwrap();
            drop(temp_file);
            let inode = fs::identity(
                &fs::open(&source_fd, "home-example-home.sealed", libc::O_RDONLY, 0)
                    .unwrap()
                    .metadata()
                    .unwrap(),
            );
            for _ in 0..2 {
                assert_eq!(
                    payload::deliver_to(
                        source.to_str().unwrap(),
                        home.to_str().unwrap(),
                        fs::uid(),
                        fs::gid(),
                        "home"
                    )
                    .unwrap(),
                    canonical
                );
            }
            assert!(!source.join("home-example-home.sealed").exists());
            let destination = home.join(".local/state/tilo/prepared-home");
            assert_eq!(
                std::fs::read(destination.join("home-example-home.sealed")).unwrap(),
                data
            );
            assert_eq!(
                fs::identity(
                    &std::fs::metadata(destination.join("home-example-home.sealed")).unwrap()
                ),
                inode
            );
            std::fs::write(
                destination.join("home-example-home.sealed"),
                [b"TILOMIG1".as_slice(), &[43; 72]].concat(),
            )
            .unwrap();
            assert!(
                payload::deliver_to(
                    source.to_str().unwrap(),
                    home.to_str().unwrap(),
                    fs::uid(),
                    fs::gid(),
                    "home"
                )
                .is_err()
            );
        }
    }

    phase production_handoff_conflicting_receiver_leaves_root_ciphertext_intact;
    {
        let data = [b"TILOMIG1".as_slice(), &[42; 72]].concat();
        for e in [flat_expectation(&data), expectation(&data)] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            let home = temp.path().join("home");
            private(&source).unwrap();
            private(&home).unwrap();
            let source_fd = fs::directory(source.to_str().unwrap()).unwrap();
            fs::publish_json(&source_fd, "expectation.json", &e, fs::uid(), fs::gid()).unwrap();
            std::fs::write(source.join("home-example-home.sealed"), &data).unwrap();
            std::fs::set_permissions(
                source.join("home-example-home.sealed"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            let dest = home.join(".local/state/tilo/prepared-home");
            private(&dest).unwrap();
            std::fs::write(
                dest.join("home-example-home.sealed"),
                [b"TILOMIG1".as_slice(), &[43; 72]].concat(),
            )
            .unwrap();
            std::fs::set_permissions(
                dest.join("home-example-home.sealed"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            assert!(
                payload::deliver_to(
                    source.to_str().unwrap(),
                    home.to_str().unwrap(),
                    fs::uid(),
                    fs::gid(),
                    "home"
                )
                .is_err()
            );
            assert_eq!(
                std::fs::read(source.join("home-example-home.sealed")).unwrap(),
                data
            );
        }
    }

    phase production_chain_rejects_unsafe_ancestor_and_replaced_inode;
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_str().unwrap();
        let fd = fs::chain(
            root,
            &["lib", "tilo", "prepared-home"],
            fs::uid(),
            fs::gid(),
        )
        .unwrap();
        std::fs::rename(temp.path().join("lib"), temp.path().join("old-lib")).unwrap();
        fs::chain(
            root,
            &["lib", "tilo", "prepared-home"],
            fs::uid(),
            fs::gid(),
        )
        .unwrap();
        assert!(fs::verify_chain(root, &["lib", "tilo", "prepared-home"], &fd, fs::uid()).is_err());
        std::fs::set_permissions(
            temp.path().join("lib"),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        assert!(
            fs::chain(
                root,
                &["lib", "tilo", "prepared-home"],
                fs::uid(),
                fs::gid()
            )
            .is_err()
        );
    }

    phase production_delivery_accepts_flat_source_and_flat_destination_retry;
    {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let home = temp.path().join("home");
        private(&source).unwrap();
        private(&home).unwrap();
        let data = [b"TILOMIG1".as_slice(), &[42; 72]].concat();
        let canonical = expectation(&data);
        let flat = flat_expectation(&data);
        let source_fd = fs::directory(source.to_str().unwrap()).unwrap();
        fs::publish_json(&source_fd, "expectation.json", &flat, fs::uid(), fs::gid()).unwrap();
        let temp_file = fs::Temporary::new(&source_fd, fs::uid(), fs::gid()).unwrap();
        std::io::Write::write_all(&mut &temp_file.fd, &data).unwrap();
        temp_file.publish("home-example-home.sealed").unwrap();
        drop(temp_file);

        assert_eq!(
            payload::deliver_to(
                source.to_str().unwrap(),
                home.to_str().unwrap(),
                fs::uid(),
                fs::gid(),
                "home"
            )
            .unwrap(),
            canonical
        );
        let destination = home.join(".local/state/tilo/prepared-home");
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(destination.join("expectation.json")).unwrap()).unwrap(),
            canonical
        );

        // Retry against an already-written matching flat expectation succeeds.
        std::fs::write(
            destination.join("expectation.json"),
            serde_json::to_vec(&flat).unwrap(),
        )
        .unwrap();
        assert_eq!(
            payload::deliver_to(
                source.to_str().unwrap(),
                home.to_str().unwrap(),
                fs::uid(),
                fs::gid(),
                "home"
            )
            .unwrap(),
            canonical
        );

        // A differing flat expectation on destination still fails closed.
        let mut mismatch = flat;
        mismatch["setup_binding"] = serde_json::json!("other setup");
        std::fs::write(
            destination.join("expectation.json"),
            serde_json::to_vec(&mismatch).unwrap(),
        )
        .unwrap();
        assert!(
            payload::deliver_to(
                source.to_str().unwrap(),
                home.to_str().unwrap(),
                fs::uid(),
                fs::gid(),
                "home"
            )
            .is_err()
        );
    }
}
