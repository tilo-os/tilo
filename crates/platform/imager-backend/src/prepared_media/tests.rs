use super::*;

#[test]
fn portable_metadata_keeps_its_wire_shape() {
    let kind = IdentityKind::EnrolledDevice {
        device_public_key: [3; 32],
    };
    let value = serde_json::to_value(&kind).unwrap();
    assert_eq!(value["kind"], "enrolled_device");
    assert_eq!(serde_json::from_value::<IdentityKind>(value).unwrap(), kind);
    assert!(
        serde_json::from_str::<PortableHomePlan>(
            r#"{"schema":1,"selection":"full_home","unknown":true}"#
        )
        .is_err()
    );
    let receipt = DeliveryReceipt {
        relative_path: PathBuf::from(DIRECTORY).join(FILE),
        sealed_bytes: 4,
        ciphertext_sha256: "ab".repeat(32),
    };
    assert_eq!(
        receipt.expectation(),
        serde_json::json!({
            "schema": 1, "sealed_bytes": 4, "ciphertext_sha256": "ab".repeat(32)
        })
    );
}

#[test]
fn manifest_bindings_reject_ambiguous_or_unbounded_accounts() {
    assert!(validate_manifest_binding(1, "setup", &["account".into()], Some(1)).is_ok());
    assert!(validate_manifest_binding(1, "setup", &[], None).is_ok());
    assert!(validate_manifest_binding(2, "setup", &[], None).is_err());
    assert!(validate_manifest_binding(1, "setup", &[], Some(1)).is_err());
    assert!(validate_manifest_binding(1, "setup", &["account".into()], Some(2)).is_err());
    assert!(validate_manifest_binding(1, "setup", &["same".into(), "same".into()], None).is_err());
    assert!(validate_manifest_binding(1, "setup", &["bad\nname".into()], None).is_err());
    assert!(validate_manifest_binding(1, &"x".repeat(257), &[], None).is_err());
    let accounts = (0..65).map(|index| index.to_string()).collect::<Vec<_>>();
    assert!(validate_manifest_binding(1, "setup", &accounts, None).is_err());
}

#[test]
fn unsupported_is_a_typed_error_and_retains_its_io_kind() {
    let error: io::Error = PreparedMediaError::Unsupported.into();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn native_admission_is_explicitly_unsupported() {
    assert!(matches!(
        PreparedMediaHandle::review(
            std::path::Path::new("/unused"),
            b"unused",
            &AtomicBool::new(false)
        ),
        Err(PreparedMediaError::Unsupported)
    ));
}

#[cfg(target_os = "linux")]
mod native_tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        io::{Read, Seek, SeekFrom},
    };
    use tilo_seal::{Context, Label, PasswordParams, SymmetricKey, envelope};

    const PASSWORD: &[u8] = b"the chosen destination password";

    fn frame(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(
            &u32::try_from(bytes.len())
                .expect("fixture frame length")
                .to_le_bytes(),
        );
        out.extend_from_slice(bytes);
    }
    fn record(prefix: &[u8], index: u64, footer: bool) -> Vec<u8> {
        let mut result = prefix.to_vec();
        result.extend_from_slice(&index.to_le_bytes());
        result.push(u8::from(footer));
        result
    }
    fn fixture(purpose: &str) -> (tempfile::TempDir, PathBuf, Vec<u8>) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("prepared.sealed");
        let mut prefix = b"TILOMIG1".to_vec();
        prefix.extend_from_slice(&[7; 32]);
        let key = SymmetricKey::generate().unwrap();
        let header = zeroize::Zeroizing::new(
            serde_json::to_vec(&serde_json::json!({
                "purpose": purpose, "key": key.expose_secret()
            }))
            .unwrap(),
        );
        let mut bytes = prefix.clone();
        frame(
            &mut bytes,
            &envelope::seal_with_password(
                PASSWORD,
                PasswordParams::DEFAULT,
                Context::new(Label::MigrationPackage, &prefix),
                &header,
            )
            .unwrap(),
        );
        let payload = vec![3; 128 * 1024];
        frame(
            &mut bytes,
            &envelope::seal(
                &key,
                Context::new(Label::MigrationPackage, &record(&prefix, 0, false)),
                &payload,
            )
            .unwrap(),
        );
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let footer = serde_json::to_vec(&serde_json::json!({
            "chunks": 1, "bytes": payload.len(),
            "sha256": <[u8; 32]>::from(Sha256::digest(&payload)),
            "runtime_sockets": 0, "setup_binding": "fixture"
        }))
        .unwrap();
        frame(
            &mut bytes,
            &envelope::seal(
                &key,
                Context::new(Label::MigrationPackage, &record(&prefix, 1, true)),
                &footer,
            )
            .unwrap(),
        );
        fs::write(&path, &bytes).unwrap();
        (temp, path, bytes)
    }

    #[test]
    fn native_admission_and_copy_use_authenticated_held_bytes() {
        let (_temp, path, bytes) = fixture("prepared_setup");
        let cancel = AtomicBool::new(false);
        let mut handle = PreparedMediaHandle::review(&path, PASSWORD, &cancel).unwrap();
        assert_eq!(handle.info().sealed_bytes, bytes.len() as u64);
        assert_eq!(
            handle.ciphertext_digest(),
            <[u8; 32]>::from(Sha256::digest(&bytes))
        );
        // Replacing the pathname must neither redirect nor invalidate held custody.
        fs::rename(&path, path.with_extension("old")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let mut progress = Vec::new();
        handle
            .copy_ciphertext_reporting(&mut output, &cancel, &mut |event| {
                progress.push(event);
                Ok(())
            })
            .unwrap();
        output.seek(SeekFrom::Start(0)).unwrap();
        let mut copied = Vec::new();
        output.read_to_end(&mut copied).unwrap();
        assert_eq!(copied, bytes);
        assert_eq!(
            progress.last().unwrap(),
            &DeliveryProgress {
                completed: bytes.len() as u64,
                total: Some(bytes.len() as u64)
            }
        );
        assert!(
            progress
                .windows(2)
                .all(|events| events[0].completed < events[1].completed)
        );
    }

    #[test]
    fn changed_ciphertext_and_cancelled_operations_fail_closed() {
        let (_temp, path, mut bytes) = fixture("prepared_setup");
        let cancel = AtomicBool::new(false);
        let mut handle = PreparedMediaHandle::review(&path, PASSWORD, &cancel).unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let error = handle
            .copy_ciphertext(&mut output, &AtomicBool::new(true))
            .unwrap_err();
        assert!(matches!(error, PreparedMediaError::Native(ref error)
            if error.kind() == io::ErrorKind::Interrupted));
        assert_eq!(output.metadata().unwrap().len(), 0);
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(handle.copy_ciphertext(&mut output, &cancel).is_err());
        assert_eq!(output.metadata().unwrap().len(), 0);
    }

    #[test]
    fn post_copy_verification_observes_changes_made_by_the_observer() {
        let (_temp, path, mut bytes) = fixture("prepared_setup");
        let cancel = AtomicBool::new(false);
        let mut handle = PreparedMediaHandle::review(&path, PASSWORD, &cancel).unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        let result = handle.copy_ciphertext_reporting(&mut output, &cancel, &mut |event| {
            if Some(event.completed) == event.total {
                fs::write(&path, &bytes)?;
            }
            Ok(())
        });
        assert!(result.is_err());
    }

    #[test]
    fn observer_cancellation_terminates_copy_before_success() {
        let (_temp, path, _) = fixture("prepared_setup");
        let cancel = AtomicBool::new(false);
        let mut handle = PreparedMediaHandle::review(&path, PASSWORD, &cancel).unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let result = handle.copy_ciphertext_reporting(&mut output, &cancel, &mut |_| {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        });
        assert!(result.is_err());
    }

    #[test]
    fn password_purpose_footer_and_trailing_data_are_authenticated() {
        let (_temp, path, bytes) = fixture("prepared_setup");
        let cancel = AtomicBool::new(false);
        assert!(PreparedMediaHandle::review(&path, b"wrong password", &cancel).is_err());
        fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(PreparedMediaHandle::review(&path, PASSWORD, &cancel).is_err());
        let mut appended = bytes;
        appended.push(0);
        fs::write(&path, appended).unwrap();
        assert!(PreparedMediaHandle::review(&path, PASSWORD, &cancel).is_err());
        let (_temp, path, _) = fixture("home");
        assert!(PreparedMediaHandle::review(&path, PASSWORD, &cancel).is_err());
    }

    #[test]
    fn source_alias_and_observer_failure_are_refused() {
        let (_temp, path, _) = fixture("prepared_setup");
        let cancel = AtomicBool::new(false);
        let mut handle = PreparedMediaHandle::review(&path, PASSWORD, &cancel).unwrap();
        let mut alias = fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert!(handle.copy_ciphertext(&mut alias, &cancel).is_err());
        handle.verify_held_ciphertext(&cancel).unwrap();
        let mut output = tempfile::tempfile().unwrap();
        assert!(
            handle
                .copy_ciphertext_reporting(&mut output, &cancel, &mut |_| {
                    Err(io::Error::other("observer stopped delivery"))
                })
                .is_err()
        );
    }

    #[test]
    fn symlink_fifo_and_oversized_inputs_never_enter_custody() {
        let (temp, path, _) = fixture("prepared_setup");
        let cancel = AtomicBool::new(false);
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(PreparedMediaHandle::review(&link, PASSWORD, &cancel).is_err());
        let directory_link = temp.path().join("directory-link");
        std::os::unix::fs::symlink(temp.path(), &directory_link).unwrap();
        assert!(
            PreparedMediaHandle::review(&directory_link.join("prepared.sealed"), PASSWORD, &cancel)
                .is_err()
        );
        let fifo = temp.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated fixture path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(PreparedMediaHandle::review(&fifo, PASSWORD, &cancel).is_err());
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_PLAINTEXT * 2).unwrap();
        assert!(PreparedMediaHandle::review(&path, PASSWORD, &cancel).is_err());
    }
}
