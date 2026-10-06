use super::super::CHILD_NAME;
use super::*;
use std::os::unix::fs::PermissionsExt;
use tilo_files::native::atomic::lease::FileLease;

fn private_fixture(temp: tempfile::TempDir) -> tempfile::TempDir {
    // tempfile's default directory mode is subject to the process umask. Only
    // this newly owned test directory is changed; production admission stays strict.
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let opened = std::fs::File::open(temp.path()).unwrap();
    assert_eq!(
        opened.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    temp
}

fn fixture() -> tempfile::TempDir {
    #[cfg(target_os = "linux")]
    if let Some(parent) = std::env::var_os("TILO_PRIVATE_ACTIONS_TEST_ROOT") {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let parent = std::path::PathBuf::from(parent);
        assert!(
            parent.is_absolute(),
            "explicit fixture root must be absolute"
        );
        let open = |path: &std::path::Path| {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .expect("existing actual fixture directory")
        };
        let _parent = open(&parent);
        let temp = private_fixture(tempfile::tempdir_in(parent).unwrap());
        let fd = open(temp.path());
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        assert_eq!(
            unsafe { libc::fstatfs(fd.as_raw_fd(), stat.as_mut_ptr()) },
            0
        );
        let stat = unsafe { stat.assume_init() };
        assert_eq!(stat.f_type as u64, 0x5846_5342, "actual XFS required");
        let metadata = fd.metadata().unwrap();
        eprintln!(
            "sender-admin XFS fixture={} device={} inode={} magic={:#x}",
            temp.path().display(),
            metadata.dev(),
            metadata.ino(),
            stat.f_type
        );
        return temp;
    }
    private_fixture(super::super::tests::admitted_temp_dir())
}
fn scope() -> SenderScope {
    SenderScope::native(uuid::Uuid::from_bytes([1; 16]), [2; 32], [3; 32]).unwrap()
}

// The production change this test requires is the actual guarded creation,
// original-directory publication/reopen/barrier, and exact envelope validation.
#[test]
fn private_actions_admin_provisions_initial_existing_root() {
    let root = fixture();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let verified = AdministrativeFile::open(
        StorageDirectory::open(root.path()).unwrap(),
        scope(),
        [4; 16],
        AdministrativeExpectation::Prepared,
    )
    .expect("real administrative carrier must provision and verify initial file");
    let facts = verified.revalidate().expect("original live file binding");
    assert_eq!(facts.instance(), &[4; 16]);
    assert_eq!(facts.sequence(), 1);
    assert_eq!(facts.revision(), 0);
    let bytes = std::fs::read(root.path().join(CHILD_NAME).join(FILE_NAME)).unwrap();
    assert_eq!(bytes.len(), 439);
    let decoded = codec::decode(&scope(), &bytes).unwrap();
    assert_eq!(decoded.payload, NativeSenderOutboxV1::empty_bytes());
    assert_eq!(facts.file_digest(), blake3::hash(&bytes).as_bytes());
    let bounded = OwnerDirectory::admit(StorageDirectory::open(root.path()).unwrap()).unwrap();
    assert!(matches!(
        FileLease::try_acquire(
            bounded.open_lease_existing(OsStr::new(LOCK_NAME)).unwrap(),
            tilo_files::native::atomic::lease::LeaseMode::Exclusive
        ),
        Err(tilo_files::native::atomic::lease::AcquireError::Busy)
    ));
    drop(verified);
    assert!(
        FileLease::try_acquire(
            bounded.open_lease_existing(OsStr::new(LOCK_NAME)).unwrap(),
            tilo_files::native::atomic::lease::LeaseMode::Exclusive,
        )
        .is_ok()
    );
}

fn open_path(
    path: &std::path::Path,
    expectation: AdministrativeExpectation,
) -> Result<AdministrativeFile, AdministrativeFailure> {
    AdministrativeFile::open(
        StorageDirectory::open(path).unwrap(),
        scope(),
        [4; 16],
        expectation,
    )
}
fn rewrite(
    path: &std::path::Path,
    instance: [u8; 16],
    sequence: u64,
    revision: u64,
    payload: &[u8],
) {
    let bytes = codec::encode(&scope(), instance, sequence, revision, payload).unwrap();
    StorageDirectory::open(&path.join(CHILD_NAME))
        .unwrap()
        .publish(
            OsStr::new(FILE_NAME),
            &bytes,
            PublishMode::Replace,
            tilo_files::native::atomic::Privacy::OwnerOnly,
        )
        .unwrap();
}
#[test]
fn private_actions_admin_preflight_refuses_zero_instance_without_effects() {
    let root = fixture();
    let error = AdministrativeFile::open(
        StorageDirectory::open(root.path()).unwrap(),
        scope(),
        [0; 16],
        AdministrativeExpectation::Prepared,
    )
    .unwrap_err();
    assert_eq!(error.code(), Code::InvalidScope);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
/// The Cloud profile (machine and incarnation bound into the envelope) uses
/// the same administrative carrier; its file never opens under another scope.
#[test]
fn private_actions_admin_cloud_profile_creates_and_is_scope_bound() {
    let root = fixture();
    let cloud = |incarnation| {
        SenderScope::cloud(
            uuid::Uuid::from_bytes([1; 16]),
            [2; 32],
            [3; 32],
            "machine",
            incarnation,
        )
        .unwrap()
    };
    let open = |scope: SenderScope, expectation| {
        AdministrativeFile::open(
            StorageDirectory::open(root.path()).unwrap(),
            scope,
            [4; 16],
            expectation,
        )
    };
    let created = open(cloud([9; 16]), AdministrativeExpectation::Prepared).unwrap();
    assert_eq!(created.revalidate().unwrap().instance(), &[4; 16]);
    drop(created);
    drop(open(cloud([9; 16]), AdministrativeExpectation::Active).unwrap());
    assert!(open(cloud([8; 16]), AdministrativeExpectation::Active).is_err());
    assert!(open(scope(), AdministrativeExpectation::Active).is_err());
}
#[test]
fn private_actions_admin_resume_initial_and_active_later_are_distinct() {
    let root = fixture();
    drop(open_path(root.path(), AdministrativeExpectation::Prepared).unwrap());
    drop(open_path(root.path(), AdministrativeExpectation::Prepared).unwrap());
    rewrite(
        root.path(),
        [4; 16],
        19,
        42,
        NativeSenderOutboxV1::empty_bytes(),
    );
    let active = open_path(root.path(), AdministrativeExpectation::Active).unwrap();
    assert_eq!(active.revalidate().unwrap().sequence(), 19);
    assert_eq!(active.revalidate().unwrap().revision(), 42);
    drop(active);
    let original = std::fs::read(root.path().join(CHILD_NAME).join(FILE_NAME)).unwrap();
    assert_eq!(
        open_path(root.path(), AdministrativeExpectation::Prepared)
            .unwrap_err()
            .code(),
        Code::InvalidFile
    );
    assert_eq!(
        std::fs::read(root.path().join(CHILD_NAME).join(FILE_NAME)).unwrap(),
        original
    );
    rewrite(
        root.path(),
        [5; 16],
        19,
        42,
        NativeSenderOutboxV1::empty_bytes(),
    );
    assert_eq!(
        open_path(root.path(), AdministrativeExpectation::Active)
            .unwrap_err()
            .code(),
        Code::InvalidFile
    );
}
#[test]
fn private_actions_admin_active_missing_never_creates_or_reseeds() {
    let root = fixture();
    assert!(open_path(root.path(), AdministrativeExpectation::Active).is_err());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    drop(open_path(root.path(), AdministrativeExpectation::Prepared).unwrap());
    let file = root.path().join(CHILD_NAME).join(FILE_NAME);
    std::fs::remove_file(&file).unwrap();
    assert!(open_path(root.path(), AdministrativeExpectation::Active).is_err());
    assert!(!file.exists());
    // An existing empty child is unexplained after lost fresh custody, even Prepared.
    assert!(open_path(root.path(), AdministrativeExpectation::Prepared).is_err());
    assert!(!file.exists());
}
#[test]
fn private_actions_admin_busy_retry_retains_original_root_and_sentinel() {
    let container = fixture();
    let owner = container.path().join("owner");
    let moved = container.path().join("moved");
    std::fs::create_dir(&owner).unwrap();
    std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700)).unwrap();
    let first = open_path(&owner, AdministrativeExpectation::Prepared).unwrap();
    let busy = open_path(&owner, AdministrativeExpectation::Prepared).unwrap_err();
    assert_eq!(busy.code(), Code::Busy);
    drop(first);
    std::fs::rename(&owner, &moved).unwrap();
    std::fs::create_dir(&owner).unwrap();
    std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700)).unwrap();
    let resumed = busy.retry().unwrap();
    assert_eq!(resumed.revalidate().unwrap().instance(), &[4; 16]);
    assert_eq!(std::fs::read_dir(&owner).unwrap().count(), 0);
    assert!(moved.join(CHILD_NAME).join(FILE_NAME).exists());
}
#[test]
fn private_actions_admin_current_root_sentinel_child_and_file_fences() {
    for which in 0..4 {
        let root = fixture();
        let held = open_path(root.path(), AdministrativeExpectation::Prepared).unwrap();
        match which {
            0 => std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755))
                .unwrap(),
            1 => StorageDirectory::open(root.path())
                .unwrap()
                .publish(
                    OsStr::new(LOCK_NAME),
                    &[],
                    PublishMode::Replace,
                    tilo_files::native::atomic::Privacy::OwnerOnly,
                )
                .unwrap(),
            2 => {
                std::fs::rename(root.path().join(CHILD_NAME), root.path().join("old-child"))
                    .unwrap();
                std::fs::create_dir(root.path().join(CHILD_NAME)).unwrap();
                std::fs::set_permissions(
                    root.path().join(CHILD_NAME),
                    std::fs::Permissions::from_mode(0o700),
                )
                .unwrap();
            }
            _ => rewrite(
                root.path(),
                [4; 16],
                1,
                0,
                NativeSenderOutboxV1::empty_bytes(),
            ),
        }
        assert!(held.revalidate().is_err());
        drop(held);
    }
}
#[test]
fn private_actions_admin_publication_failures_retain_exact_custody() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure_on_nth};
    for occurrence in [1, 2] {
        for checkpoint in [
            Checkpoint::BeforePublish,
            Checkpoint::BeforeCommit,
            Checkpoint::AfterPublish,
        ] {
            let root = fixture();
            let error = with_failure_on_nth(checkpoint, occurrence, || {
                open_path(root.path(), AdministrativeExpectation::Prepared)
            })
            .unwrap_err();
            let expected = match checkpoint {
                Checkpoint::BeforePublish => PublicationState::NotPublished,
                Checkpoint::BeforeCommit => PublicationState::PublishedDurabilityUnknown,
                _ => PublicationState::PublicationUnknown,
            };
            assert_eq!(error.publication_error().unwrap().state(), expected);
            if occurrence == 2 {
                let bounded =
                    OwnerDirectory::admit(StorageDirectory::open(root.path()).unwrap()).unwrap();
                assert!(matches!(
                    FileLease::try_acquire(
                        bounded.open_lease_existing(OsStr::new(LOCK_NAME)).unwrap(),
                        LeaseMode::Exclusive
                    ),
                    Err(AcquireError::Busy)
                ));
            }
            if expected == PublicationState::PublicationUnknown {
                let still_unknown = error.retry().unwrap_err();
                assert_eq!(still_unknown.publication_error().unwrap().state(), expected);
                drop(still_unknown);
                // Actual restart re-proves current bytes; no error is converted to success.
                drop(open_path(root.path(), AdministrativeExpectation::Prepared).unwrap());
            } else {
                let complete = error.retry().unwrap();
                assert_eq!(complete.revalidate().unwrap().sequence(), 1);
            }
        }
    }
}
#[test]
fn private_actions_admin_lost_fresh_custody_does_not_adopt_partial_child() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure_on_nth};
    let root = fixture();
    let error = with_failure_on_nth(Checkpoint::BeforePublish, 2, || {
        open_path(root.path(), AdministrativeExpectation::Prepared)
    })
    .unwrap_err();
    assert_eq!(
        error.publication_error().unwrap().state(),
        PublicationState::NotPublished
    );
    drop(error);
    assert!(root.path().join(CHILD_NAME).is_dir());
    assert!(open_path(root.path(), AdministrativeExpectation::Prepared).is_err());
    assert!(!root.path().join(CHILD_NAME).join(FILE_NAME).exists());
}
#[test]
fn private_actions_admin_existing_barrier_and_named_file_change_keep_original_error() {
    use tilo_files::native::atomic::test_support::{ExistingCheckpoint, with_existing_hooks};
    let root = fixture();
    let error = with_existing_hooks(
        Some(ExistingCheckpoint::BeforeNamespaceFlush),
        |_| {},
        || open_path(root.path(), AdministrativeExpectation::Prepared),
    )
    .unwrap_err();
    assert!(error.existing_commit_error().is_some());
    drop(error.retry().unwrap());
    let file = root.path().join(CHILD_NAME).join(FILE_NAME);
    let backup = root.path().join(CHILD_NAME).join("old-file");
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let error = with_existing_hooks(
        None,
        move |at| {
            if at == ExistingCheckpoint::BeforeDataFlush {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    std::fs::rename(&file, &backup).unwrap();
                    std::fs::copy(&backup, &file).unwrap();
                }
            }
        },
        || open_path(root.path(), AdministrativeExpectation::Active),
    )
    .unwrap_err();
    assert!(error.existing_commit_error().unwrap().source_changed());
    let refused = error.retry().unwrap_err();
    assert!(refused.existing_commit_error().is_some());
    assert_eq!(refused.code(), Code::InvalidFile);
}
#[test]
fn private_actions_admin_real_allocations_are_released_on_drop() {
    let root = fixture();
    let original = StorageDirectory::open(root.path()).unwrap();
    let ((), stats) = crate::semantic_document_read::retirement::measure_test_allocations(|| {
        let held = AdministrativeFile::open(
            original,
            scope(),
            [4; 16],
            AdministrativeExpectation::Prepared,
        )
        .unwrap();
        assert_eq!(held.revalidate().unwrap().sequence(), 1);
        drop(held);
    });
    assert_eq!(stats.live, 0);
}
#[test]
fn private_actions_admin_bounded_corrupt_read_preserves_file() {
    let root = fixture();
    drop(open_path(root.path(), AdministrativeExpectation::Prepared).unwrap());
    let path = root.path().join(CHILD_NAME).join(FILE_NAME);
    let malformed = vec![7; codec::MAX_FILE + 2];
    std::fs::write(&path, &malformed).unwrap();
    let original = StorageDirectory::open(root.path()).unwrap();
    let ((), stats) = crate::semantic_document_read::retirement::measure_test_allocations(|| {
        let failed = AdministrativeFile::open(
            original,
            scope(),
            [4; 16],
            AdministrativeExpectation::Active,
        )
        .unwrap_err();
        assert_eq!(failed.code(), Code::InvalidFile);
        drop(failed);
    });
    assert_eq!(stats.live, 0);
    assert_eq!(std::fs::read(path).unwrap(), malformed);
}

// Independent inert file bytes exercise the real unchanged managed decoder;
// this creates no admitted row, committed receipt, or result capability.
fn observed_payload(json: &str) -> Vec<u8> {
    fn short(bytes: &mut Vec<u8>, text: &str) {
        bytes.extend_from_slice(&(text.len() as u16).to_le_bytes());
        bytes.extend_from_slice(text.as_bytes());
    }
    let mut row = vec![8; 16];
    row.extend_from_slice(&[1; 32]);
    row.extend_from_slice(&[2; 32]);
    short(&mut row, "caller");
    short(&mut row, "record");
    row.extend_from_slice(&u16::MAX.to_le_bytes());
    row.extend_from_slice(&[3; 16]);
    row.extend_from_slice(&7u64.to_le_bytes());
    short(&mut row, "target");
    short(&mut row, "record");
    row.extend_from_slice(&[4; 32]);
    row.extend_from_slice(&[5; 32]);
    row.extend_from_slice(&[6; 32]);
    short(&mut row, "revision");
    short(&mut row, "resource");
    short(&mut row, "action");
    row.extend_from_slice(&u32::MAX.to_le_bytes());
    row.push(1);
    row.extend_from_slice(&9u64.to_le_bytes());
    row.extend_from_slice(&[0, 0]);
    row.extend_from_slice(&(json.len() as u32).to_le_bytes());
    row.extend_from_slice(json.as_bytes());
    let mut bytes = b"NSO1\x01\x00\x01\x00".to_vec();
    bytes.extend_from_slice(&(row.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&row);
    bytes
}
#[test]
fn private_actions_admin_active_nonempty_observation_parser_is_released() {
    let root = fixture();
    drop(open_path(root.path(), AdministrativeExpectation::Prepared).unwrap());
    let escaped = "\\n".repeat(100_000);
    let valid = format!(
        r#"{{"state":"succeeded","invocation":"08080808-0808-0808-0808-080808080808","output":{{"value":"{escaped}"}}}}"#
    );
    let invalid = format!(
        r#"{{"state":"accepted","invocation":"08080808-0808-0808-0808-080808080808","{escaped}":0}}"#
    );
    for (json, expected) in [(&valid, true), (&invalid, false)] {
        let payload = observed_payload(json);
        rewrite(root.path(), [4; 16], 27, 3, &payload);
        let original_bytes = std::fs::read(root.path().join(CHILD_NAME).join(FILE_NAME)).unwrap();
        let original = StorageDirectory::open(root.path()).unwrap();
        let ((), stats) =
            crate::semantic_document_read::retirement::measure_test_allocations(|| {
                let result = AdministrativeFile::open(
                    original,
                    scope(),
                    [4; 16],
                    AdministrativeExpectation::Active,
                );
                if expected {
                    let file = result.unwrap();
                    assert_eq!(file.revalidate().unwrap().sequence(), 27);
                    drop(file);
                } else {
                    assert_eq!(result.unwrap_err().code(), Code::InvalidFile);
                }
            });
        assert_eq!(stats.live, 0);
        assert_eq!(
            std::fs::read(root.path().join(CHILD_NAME).join(FILE_NAME)).unwrap(),
            original_bytes
        );
    }
}
#[test]
fn private_actions_admin_same_inode_content_change_refuses_current_facts() {
    let root = fixture();
    let held = open_path(root.path(), AdministrativeExpectation::Prepared).unwrap();
    let path = root.path().join(CHILD_NAME).join(FILE_NAME);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let identity = FileIdentity::of(&file).unwrap();
    let changed =
        codec::encode(&scope(), [4; 16], 2, 1, NativeSenderOutboxV1::empty_bytes()).unwrap();
    std::fs::write(&path, changed).unwrap();
    assert_eq!(FileIdentity::of(&file).unwrap(), identity);
    let error = held.revalidate().unwrap_err();
    assert_eq!(error.code(), Code::InvalidFile);
    drop(error);
    drop(held);
}
#[test]
fn private_actions_admin_active_missing_sentinel_or_child_preserves_absence() {
    for sentinel in [true, false] {
        let root = fixture();
        drop(open_path(root.path(), AdministrativeExpectation::Prepared).unwrap());
        let path = root
            .path()
            .join(if sentinel { LOCK_NAME } else { CHILD_NAME });
        if sentinel {
            std::fs::remove_file(&path).unwrap();
        } else {
            std::fs::rename(&path, root.path().join("preserved-child")).unwrap();
        }
        assert!(open_path(root.path(), AdministrativeExpectation::Active).is_err());
        assert!(!path.exists());
    }
}

#[test]
fn private_actions_admin_owned_recheck_retains_actual_error_and_lease() {
    for changed_child in [false, true] {
        let root = fixture();
        let file = open_path(root.path(), AdministrativeExpectation::Prepared).unwrap();
        let child = root.path().join(CHILD_NAME);
        let moved = root.path().join("retained-original-child");
        let path = child.join(FILE_NAME);
        let original_bytes = std::fs::read(&path).unwrap();
        if changed_child {
            std::fs::rename(&child, &moved).unwrap();
            std::fs::create_dir(&child).unwrap();
            std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o755)).unwrap();
        } else {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .write_all(b"changed")
                .unwrap();
        }
        let (failure, observed) =
            crate::semantic_document_read::retirement::measure_test_allocations(|| {
                file.revalidate_owned().unwrap_err()
            });
        assert_eq!(
            observed.live, 0,
            "owned transfer itself allocates no backing"
        );
        assert_eq!(observed.allocations, 0);
        assert_eq!(
            failure.io_error().unwrap().kind(),
            if changed_child {
                std::io::ErrorKind::PermissionDenied
            } else {
                std::io::ErrorKind::InvalidData
            }
        );
        if changed_child {
            assert!(
                matches!(
                    failure.operation.diagnostic.as_ref(),
                    Some(OpenError::Child(
                        tilo_files::native::atomic::bounded::ChildOpenError::Admission(_)
                    ))
                ),
                "failed child FD must remain inside the actual admission error"
            );
        }
        let bounded = OwnerDirectory::admit(StorageDirectory::open(root.path()).unwrap()).unwrap();
        assert!(matches!(
            FileLease::try_acquire(
                bounded.open_lease_existing(OsStr::new(LOCK_NAME)).unwrap(),
                tilo_files::native::atomic::lease::LeaseMode::Exclusive
            ),
            Err(tilo_files::native::atomic::lease::AcquireError::Busy)
        ));
        let failure = failure.retry().unwrap_err();
        assert!(failure.io_error().is_some());
        if changed_child {
            std::fs::remove_dir(&child).unwrap();
            std::fs::rename(&moved, &child).unwrap();
        } else {
            std::fs::write(&path, &original_bytes).unwrap();
        }
        let recovered = failure.retry().unwrap();
        assert_eq!(recovered.revalidate().unwrap().sequence(), 1);
        drop(recovered);
        assert!(
            FileLease::try_acquire(
                bounded.open_lease_existing(OsStr::new(LOCK_NAME)).unwrap(),
                tilo_files::native::atomic::lease::LeaseMode::Exclusive
            )
            .is_ok()
        );
    }
}
