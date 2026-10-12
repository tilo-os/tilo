use super::*;

/// Raw ancestry and effective ACL mask parsing.
#[test]
fn linux_ancestry_and_acl_mask_rules_are_lossless_and_strict() {
    // Phase `raw_ancestry_is_bounded_lossless_and_refuses_normalization`.
    {
        assert_eq!(
            raw_components(b"/home/user/\xff/cache").unwrap(),
            vec![&b"home"[..], &b"user"[..], &b"\xff"[..], &b"cache"[..]]
        );
        for value in [
            &b"relative"[..],
            b"/a/../b",
            b"/a/./b",
            b"/a//b",
            b"/a/",
            b"/a\0b",
            b"/",
        ] {
            assert!(raw_components(value).is_err(), "accepted {value:?}");
        }
        assert!(raw_components(format!("/{}", vec!["a"; 64].join("/")).as_bytes()).is_ok());
        assert!(raw_components(format!("/{}", vec!["a"; 65].join("/")).as_bytes()).is_err());
        assert!(raw_components(format!("/{}", "a".repeat(255)).as_bytes()).is_ok());
        assert!(raw_components(format!("/{}", "a".repeat(256)).as_bytes()).is_err());
        let mut leaves = vec!["a".repeat(240); 17];
        leaves[16] = "a".repeat(239);
        let at_limit = format!("/{}", leaves.join("/"));
        assert_eq!(at_limit.len(), 4096);
        assert!(raw_components(at_limit.as_bytes()).is_ok());
        leaves[16].push('a');
        let over_limit = format!("/{}", leaves.join("/"));
        assert_eq!(over_limit.len(), 4097);
        assert!(raw_components(over_limit.as_bytes()).is_err());
    }

    // Phase `effective_acl_mask_permits_safe_named_grants_and_refuses_effective_access`.
    {
        // Linux POSIX ACL named entries are bounded by these group-class mask bits.
        // A named rwx entry with mask --- is safe on a private directory/file.
        assert!(effective_mode_allowed(1000, 1000, 0o700, true, true));
        assert!(effective_mode_allowed(1000, 1000, 0o600, false, true));
        assert!(effective_mode_allowed(0, 1000, 0o755, true, false));
        assert!(effective_mode_allowed(1000, 1000, 0o750, true, false));
        for mode in [0o770, 0o720, 0o707, 0o1700] {
            assert!(!effective_mode_allowed(1000, 1000, mode, true, true));
        }
        assert!(!effective_mode_allowed(1000, 1000, 0o640, false, true));
        assert!(!effective_mode_allowed(0, 1000, 0o775, true, false));
        assert!(!effective_mode_allowed(1001, 1000, 0o700, true, false));
        assert!(!effective_mode_allowed(0, 1000, 0o700, true, true));
    }
}

// These fixtures exercise actual Linux syscalls. They are source-checked only
// under the current runtime holds; host tests above make no native claim.
#[cfg(target_os = "linux")]
mod native {
    use super::super::{native::fixture_acquire, *};
    use crate::setup::internal_cache::layout;
    use crate::setup::test_support::isolated;
    use crate::{
        release::InternalRelease,
        setup::{
            internal_bundle,
            internal_cache::{Candidate, Kind, Provider, Session},
            internal_download::SelectedManifest,
        },
    };
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        os::unix::{
            ffi::OsStringExt,
            fs::{OpenOptionsExt, PermissionsExt},
        },
        sync::atomic::AtomicBool,
    };
    fn selected() -> SelectedManifest {
        let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
        let release = InternalRelease::transport_fixture(
            &bytes,
            "https://code.tilo.so/releases/manifest.json",
        );
        SelectedManifest::from_bytes(&release, bytes).unwrap()
    }
    fn directory() -> tempfile::TempDir {
        // No /tmp fallback: native fixtures need a qualified filesystem, and
        // never the real cache of the account running the tests.
        crate::setup::test_support::qualified_tempdir()
    }
    fn root(path: &std::path::Path, s: &SelectedManifest) -> std::path::PathBuf {
        path.join("tilo-internal-install").join(s.manifest_sha256())
    }
    fn parts<'a>(path: &std::path::Path, s: &'a SelectedManifest) -> [Partial<'a>; 2] {
        fixture_acquire(path, s)
            .unwrap()
            .bootstrap(s, &AtomicBool::new(false))
            .unwrap()
            .map(|c| match c {
                Component::Partial(p) => p,
                _ => panic!("expected partial"),
            })
    }
    /// Fresh and cancelled bootstrap, zero-record residue and visitor errors.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn linux_bootstrap_and_scan_settle_before_cancellation_or_failure() {
        isolated(
            "fresh_bootstrap_settles_before_cancellation_or_second_acquisition_failure",
            || {
                for stage in ["partial-durable", "record-created", "component-acquire-1"] {
                    let dir = directory();
                    let s = selected();
                    let cancel = std::sync::Arc::new(AtomicBool::new(false));
                    if stage == "component-acquire-1" {
                        super::FAIL_AT.with(|v| *v.borrow_mut() = Some(stage));
                    } else {
                        super::CANCEL_AT.with(|v| *v.borrow_mut() = Some((stage, cancel.clone())));
                    }
                    let result = fixture_acquire(dir.path(), &s)
                        .unwrap()
                        .bootstrap(&s, &cancel);
                    super::FAIL_AT.with(|v| *v.borrow_mut() = None);
                    super::CANCEL_AT.with(|v| *v.borrow_mut() = None);
                    assert!(result.is_err());
                    assert_eq!(
                        cancel.load(std::sync::atomic::Ordering::Acquire),
                        stage != "component-acquire-1"
                    );
                    let namespace = root(dir.path(), &s);
                    let payload = namespace.join("payload");
                    let names: Vec<_> = fs::read_dir(&payload)
                        .unwrap()
                        .map(|e| e.unwrap().file_name())
                        .collect();
                    assert_eq!(names.len(), 1);
                    assert_eq!(fs::read(payload.join(&names[0])).unwrap(), b"");
                    let zero = fs::read(namespace.join("journal-0/record-0000000001")).unwrap();
                    let record =
                        crate::setup::internal_cache::checkpoint::Record::decode(&zero).unwrap();
                    assert_eq!(
                        record.hint_for(&s, 0).unwrap().leaf.as_bytes(),
                        names[0].as_encoded_bytes()
                    );
                    let again = selected(); // exact reauthentication after all owners were dropped
                    let [mut a, mut b] = parts(dir.path(), &again);
                    assert_eq!(
                        a.provider.ownership().leaf.as_bytes(),
                        names[0].as_encoded_bytes()
                    );
                    assert!(a.provider.retain(&[]).is_err());
                    prepare(&mut a, &again, 0);
                    prepare(&mut b, &again, 1);
                    assert_eq!(
                        fs::read(namespace.join("journal-0/record-0000000001")).unwrap(),
                        zero
                    );
                }
            },
        );

        isolated(
            "cancelled_bootstrap_creates_no_payload_and_settlement_is_not_preparation",
            || {
                let dir = directory();
                let s = selected();
                let cancel = AtomicBool::new(true);
                assert!(
                    fixture_acquire(dir.path(), &s)
                        .unwrap()
                        .bootstrap(&s, &cancel)
                        .is_err()
                );
                assert_eq!(
                    fs::read_dir(root(dir.path(), &s).join("payload"))
                        .unwrap()
                        .count(),
                    0
                );
                let [mut a, b] = parts(dir.path(), &s);
                assert!(a.provider.retain(&[]).is_err());
                // Full authenticated bytes isolate the preparation gate: failure must
                // not be attributable to an empty/short/corrupt component.
                let namespace = root(dir.path(), &s);
                let partial_path = namespace.join("payload").join(a.provider.ownership().leaf);
                let final_path = namespace
                    .join("payload")
                    .join(s.bundle().component(0).unwrap().filename);
                let zero_path = namespace.join("journal-0/record-0000000001");
                let zero = fs::read(&zero_path).unwrap();
                let bytes = vec![b'E'; s.bundle().component(0).unwrap().byte_size as usize];
                a.file.write_all(&bytes).unwrap();
                a.file.sync_all().unwrap();
                assert!(a.publish(&AtomicBool::new(false)).is_err());
                assert_eq!(fs::read(&partial_path).unwrap(), bytes);
                assert_eq!(fs::read(&zero_path).unwrap(), zero);
                assert!(!final_path.exists());
                drop(b);
                let [mut a, b] = parts(dir.path(), &s);
                finish(&mut a, &s, 0);
                drop(a.publish(&AtomicBool::new(false)).unwrap());
                drop(b);
                // Remove only this test's known component-1 fixture to exercise final/fresh.
                let namespace = root(dir.path(), &s);
                let record_path = namespace.join("journal-1/record-0000000001");
                let bytes = fs::read(&record_path).unwrap();
                let record =
                    crate::setup::internal_cache::checkpoint::Record::decode(&bytes).unwrap();
                fs::remove_file(
                    namespace
                        .join("payload")
                        .join(record.hint_for(&s, 1).unwrap().leaf),
                )
                .unwrap();
                fs::remove_file(record_path).unwrap();
                let [first, second] = fixture_acquire(dir.path(), &s)
                    .unwrap()
                    .bootstrap(&s, &AtomicBool::new(false))
                    .unwrap();
                assert!(matches!(first, Component::Final(_)));
                assert!(matches!(second, Component::Partial(_)));
                assert!(namespace.join("journal-1/record-0000000001").exists());
            },
        );

        isolated(
            "creation_before_zero_record_is_unknown_residue_even_when_empty",
            || {
                let dir = directory();
                let s = selected();
                super::FAIL_AT.with(|v| *v.borrow_mut() = Some("partial-durable"));
                assert!(
                    fixture_acquire(dir.path(), &s)
                        .unwrap()
                        .bootstrap(&s, &AtomicBool::new(false))
                        .is_err()
                );
                super::FAIL_AT.with(|v| *v.borrow_mut() = None);
                assert!(
                    fixture_acquire(dir.path(), &s)
                        .unwrap()
                        .bootstrap(&s, &AtomicBool::new(false))
                        .is_err()
                );
                assert_eq!(
                    fs::read_dir(root(dir.path(), &s).join("payload"))
                        .unwrap()
                        .count(),
                    1
                );
            },
        );

        isolated("fresh_scan_stops_immediately_on_visitor_error", || {
            let dir = directory();
            let s = selected();
            let [mut a, mut b] = parts(dir.path(), &s);
            prepare(&mut a, &s, 0);
            prepare(&mut b, &s, 1);
            let mut calls = 0;
            assert!(
                a.provider
                    .scan(&mut |_| {
                        calls += 1;
                        Err("visitor stop".into())
                    })
                    .is_err()
            );
            assert_eq!(calls, 1);
            assert!(a.provider.retain(&[]).is_err());
            assert_eq!(
                fs::read_dir(root(dir.path(), &s).join("journal-0"))
                    .unwrap()
                    .count(),
                1
            );
        });
    }
    fn prepare(part: &mut Partial<'_>, s: &SelectedManifest, index: usize) {
        let mut session = Session::new(s, index, 1, &mut part.provider).unwrap();
        drop(
            session
                .prepare(s, &mut part.file, &AtomicBool::new(false))
                .unwrap(),
        );
    }
    fn finish(part: &mut Partial<'_>, s: &SelectedManifest, index: usize) {
        let mut session = Session::new(s, index, 2, &mut part.provider).unwrap();
        let mut prepared = session
            .prepare(s, &mut part.file, &AtomicBool::new(false))
            .unwrap();
        let value = if index == 0 { b'E' } else { b'R' };
        let bytes = vec![value; s.bundle().component(index).unwrap().byte_size as usize];
        prepared.file.write_all(&bytes).unwrap();
        prepared.hashes.written(&bytes);
        prepared
            .hashes
            .verify_tail(prepared.file, &AtomicBool::new(false))
            .unwrap();
        prepared
            .observer
            .begin_attempt(prepared.id, prepared.file)
            .unwrap();
        prepared
            .observer
            .boundary(
                prepared.id,
                prepared.file,
                Candidate {
                    length: bytes.len() as u64,
                    digest: prepared.hashes.digest(),
                    kind: Kind::Final,
                },
            )
            .unwrap();
    }
    /// Process lock, non-UTF-8 ancestry, hardlink substitution, changed ancestors and ACLs.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn linux_locks_and_ancestry_refuse_substitution_and_keep_names() {
        isolated(
            "independent_process_lock_is_retained_by_either_component_and_final",
            || {
                let dir = directory();
                let s = selected();
                let [mut a, mut b] = parts(dir.path(), &s);
                prepare(&mut b, &s, 1);
                finish(&mut a, &s, 0);
                let final_file = a.publish(&AtomicBool::new(false)).unwrap();
                assert!(!final_file.has_retained_residue());
                drop(b);
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "setup::internal_cache::linux::tests::native::lock_probe",
                        "--ignored",
                    ])
                    .env("TILO_CACHE_LOCK_PROBE", dir.path())
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                let stdout = String::from_utf8_lossy(&output.stdout);
                // libtest exits 0 when `--exact` selects nothing; require the child ran.
                assert!(
                    stdout.contains("test result: ok. 1 passed"),
                    "the child ran no test: {stdout}"
                );
                drop(final_file);
                assert!(fixture_acquire(dir.path(), &s).is_ok());
            },
        );

        isolated("non_utf8_cache_ancestry_survives_close_and_resume", || {
            let dir = directory();
            let path = dir
                .path()
                .join(std::ffi::OsString::from_vec(b"cache-\xff".to_vec()));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            let s = selected();
            let [mut a, mut b] = parts(&path, &s);
            prepare(&mut b, &s, 1);
            {
                let mut session = Session::new(&s, 0, 1, &mut a.provider).unwrap();
                let mut prepared = session
                    .prepare(&s, &mut a.file, &AtomicBool::new(false))
                    .unwrap();
                crate::setup::internal_cache::tests::start_prefix(&mut prepared, b"EEEE");
            }
            drop(a);
            drop(b);
            let [mut a, b] = parts(&path, &s);
            let mut session = Session::new(&s, 0, 2, &mut a.provider).unwrap();
            let prepared = session
                .prepare(&s, &mut a.file, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(prepared.committed(), 4);
            drop(prepared);
            drop(session);
            drop(b);
        });

        isolated(
            "unsafe_ancestor_hardlink_substitution_and_unknown_residue_refuse_untouched",
            || {
                for case in 0..5 {
                    let dir = directory();
                    let s = selected();
                    let [mut a, mut b] = parts(dir.path(), &s);
                    prepare(&mut a, &s, 0);
                    prepare(&mut b, &s, 1);
                    let leaf = a.provider.ownership().leaf;
                    let payload = root(dir.path(), &s).join("payload");
                    let path = payload.join(&leaf);
                    drop(a);
                    drop(b);
                    match case {
                        0 => {
                            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777))
                                .unwrap();
                        }
                        1 => {
                            fs::hard_link(&path, dir.path().join("alias")).unwrap();
                        }
                        2 => {
                            fs::rename(&path, dir.path().join("saved")).unwrap();
                            OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .mode(0o600)
                                .open(&path)
                                .unwrap();
                        }
                        3 => {
                            OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .mode(0o600)
                                .open(payload.join("orphan"))
                                .unwrap();
                        }
                        4 => {
                            fs::write(root(dir.path(), &s).join("journal-0/torn"), b"unknown")
                                .unwrap();
                        }
                        _ => unreachable!(),
                    }
                    let before = fs::read_dir(&payload).unwrap().count();
                    let result = fixture_acquire(dir.path(), &s)
                        .and_then(|r| r.bootstrap(&s, &AtomicBool::new(false)));
                    assert!(result.is_err(), "case {case}");
                    assert_eq!(before, fs::read_dir(&payload).unwrap().count());
                    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
                }
            },
        );

        isolated(
            "changed_held_ancestor_and_excessive_entries_refuse_without_cleanup",
            || {
                let dir = directory();
                let s = selected();
                let [mut a, mut b] = parts(dir.path(), &s);
                prepare(&mut a, &s, 0);
                prepare(&mut b, &s, 1);
                let manifest = root(dir.path(), &s);
                fs::rename(&manifest, dir.path().join("saved-root")).unwrap();
                fs::create_dir(&manifest).unwrap();
                fs::set_permissions(&manifest, fs::Permissions::from_mode(0o700)).unwrap();
                let own = a.provider.ownership();
                assert!(a.provider.verify(&a.file, &own).is_err());
                drop(a);
                drop(b);
                let dir = directory();
                let s = selected();
                let [mut a, mut b] = parts(dir.path(), &s);
                prepare(&mut a, &s, 0);
                prepare(&mut b, &s, 1);
                drop(a);
                drop(b);
                let payload = root(dir.path(), &s).join("payload");
                for i in 0..17 {
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(payload.join(format!("unknown-{i}")))
                        .unwrap();
                }
                assert!(
                    fixture_acquire(dir.path(), &s)
                        .unwrap()
                        .bootstrap(&s, &AtomicBool::new(false))
                        .is_err()
                );
                assert_eq!(fs::read_dir(payload).unwrap().count(), 19);
            },
        );

        isolated(
            "safe_effective_acl_and_inherited_default_acl_are_supported",
            || {
                use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
                fn acl(owner: u16, mask: u16) -> Vec<u8> {
                    let mut bytes = 2u32.to_le_bytes().to_vec();
                    let uid = unsafe { libc::geteuid() };
                    for (tag, perm, id) in [
                        (1u16, owner, u32::MAX),
                        (2, 7, uid + 1),
                        (4, 0, u32::MAX),
                        (16, mask, u32::MAX),
                        (32, 0, u32::MAX),
                    ] {
                        bytes.extend_from_slice(&tag.to_le_bytes());
                        bytes.extend_from_slice(&perm.to_le_bytes());
                        bytes.extend_from_slice(&id.to_le_bytes());
                    }
                    bytes
                }
                fn set(file: &std::fs::File, key: &std::ffi::CStr, bytes: &[u8]) {
                    assert_eq!(
                        unsafe {
                            libc::fsetxattr(
                                file.as_raw_fd(),
                                key.as_ptr(),
                                bytes.as_ptr().cast(),
                                bytes.len(),
                                0,
                            )
                        },
                        0
                    );
                }
                fn get(path: &std::path::Path, key: &std::ffi::CStr) -> Vec<u8> {
                    let file = std::fs::File::open(path).unwrap();
                    let mut bytes = [0u8; 256];
                    let count = unsafe {
                        libc::fgetxattr(
                            file.as_raw_fd(),
                            key.as_ptr(),
                            bytes.as_mut_ptr().cast(),
                            bytes.len(),
                        )
                    };
                    assert!(count >= 0, "{}", std::io::Error::last_os_error());
                    bytes[..count as usize].to_vec()
                }
                // Independent ordinary no-ACL positive control.
                let ordinary = directory();
                let held = std::fs::File::open(ordinary.path()).unwrap();
                for key in [c"system.posix_acl_access", c"system.posix_acl_default"] {
                    let rc = unsafe { libc::fremovexattr(held.as_raw_fd(), key.as_ptr()) };
                    assert!(
                        rc == 0
                            || std::io::Error::last_os_error().raw_os_error()
                                == Some(libc::ENODATA)
                    );
                }
                let s = selected();
                let components = fixture_acquire(ordinary.path(), &s)
                    .unwrap()
                    .bootstrap(&s, &AtomicBool::new(false))
                    .unwrap();
                drop(components);
                // Access-only safe mask and unsafe effective-write mask use fresh roots.
                for mask in [0u16, 2] {
                    let dir = directory();
                    let held = std::fs::File::open(dir.path()).unwrap();
                    set(&held, c"system.posix_acl_access", &acl(7, mask));
                    let result = fixture_acquire(dir.path(), &s);
                    assert_eq!(result.is_ok(), mask == 0);
                }
                // Install the default BEFORE the provider has created any descendants.
                // Its broad named grant is reduced by the actual private create modes.
                let inherited = directory();
                let held = std::fs::File::open(inherited.path()).unwrap();
                assert!(!inherited.path().join("tilo-internal-install").exists());
                let default = acl(7, 7);
                set(&held, c"system.posix_acl_default", &default);
                assert!(!inherited.path().join("tilo-internal-install").exists());
                let cache = fixture_acquire(inherited.path(), &s).unwrap();
                let payload = root(inherited.path(), &s).join("payload");
                for dir in [
                    inherited.path().join("tilo-internal-install"),
                    root(inherited.path(), &s),
                    payload.clone(),
                ] {
                    assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
                    assert_eq!(get(&dir, c"system.posix_acl_access"), acl(7, 0));
                    assert_eq!(get(&dir, c"system.posix_acl_default"), default);
                }
                let components = cache.bootstrap(&s, &AtomicBool::new(false)).unwrap();
                for component in components {
                    let Component::Partial(part) = component else {
                        panic!("expected inherited partial")
                    };
                    let path = payload.join(part.provider.ownership().leaf);
                    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
                    assert_eq!(get(&path, c"system.posix_acl_access"), acl(6, 0));
                }
            },
        );
    }
    #[test]
    #[ignore = "subprocess fixture invoked with a held parent lease"]
    fn lock_probe() {
        let dir = std::env::var_os("TILO_CACHE_LOCK_PROBE").expect("parent fixture path");
        let result = fixture_acquire(std::path::Path::new(&dir), &selected());
        assert!(matches!(result,Err(e) if e=="Cache Busy"));
    }
    /// Collisions, durability faults, crash boundaries, cancellation and reset fences.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn linux_record_durability_and_crash_recovery_never_claim_success() {
        isolated(
            "no_replace_collision_authenticates_winner_and_preserves_invalid_leaf",
            || {
                for valid in [false, true] {
                    let dir = directory();
                    let s = selected();
                    let [mut a, mut b] = parts(dir.path(), &s);
                    prepare(&mut b, &s, 1);
                    finish(&mut a, &s, 0);
                    let own = a.provider.ownership();
                    let payload = root(dir.path(), &s).join("payload");
                    let final_path = payload.join(s.bundle().component(0).unwrap().filename);
                    let bytes = vec![if valid { b'E' } else { b'X' }; 4096];
                    let mut winner = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&final_path)
                        .unwrap();
                    winner.write_all(&bytes).unwrap();
                    winner.sync_all().unwrap();
                    let result = a.publish(&AtomicBool::new(false));
                    assert_eq!(result.is_ok(), valid);
                    if let Ok(final_file) = result {
                        assert!(final_file.has_retained_residue());
                    }
                    assert_eq!(fs::read(&final_path).unwrap(), bytes);
                    assert!(payload.join(own.leaf).exists());
                    drop(b);
                }
            },
        );

        isolated(
            "record_durability_faults_leave_bounded_residue_and_never_claim_success",
            || {
                for stage in ["record-created", "record-written", "record-durable"] {
                    let dir = directory();
                    let s = selected();
                    super::FAIL_AT.with(|v| *v.borrow_mut() = Some(stage));
                    assert!(
                        fixture_acquire(dir.path(), &s)
                            .unwrap()
                            .bootstrap(&s, &AtomicBool::new(false))
                            .is_err()
                    );
                    super::FAIL_AT.with(|v| *v.borrow_mut() = None);
                    let result = fixture_acquire(dir.path(), &s)
                        .unwrap()
                        .bootstrap(&s, &AtomicBool::new(false));
                    if stage == "record-created" {
                        assert!(result.is_err());
                    } else {
                        assert!(result.is_ok());
                    }
                    assert_eq!(
                        fs::read_dir(root(dir.path(), &s).join("journal-0"))
                            .unwrap()
                            .count(),
                        1
                    );
                }
            },
        );

        isolated(
            "final_crash_boundaries_recover_final_before_stale_partial_records",
            || {
                for stage in ["final-renamed", "final-durable"] {
                    let dir = directory();
                    let s = selected();
                    let [mut a, mut b] = parts(dir.path(), &s);
                    prepare(&mut b, &s, 1);
                    finish(&mut a, &s, 0);
                    super::FAIL_AT.with(|v| *v.borrow_mut() = Some(stage));
                    assert!(a.publish(&AtomicBool::new(false)).is_err());
                    super::FAIL_AT.with(|v| *v.borrow_mut() = None);
                    drop(b);
                    let [a, b] = fixture_acquire(dir.path(), &s)
                        .unwrap()
                        .bootstrap(&s, &AtomicBool::new(false))
                        .unwrap();
                    match a {
                        Component::Final(mut f) => f.verify(&AtomicBool::new(false)).unwrap(),
                        _ => panic!("final recovery required"),
                    };
                    drop(b);
                }
            },
        );

        isolated(
            "cancellation_preserves_owned_prefix_and_discard_is_explicit",
            || {
                let dir = directory();
                let s = selected();
                let [mut a, mut b] = parts(dir.path(), &s);
                prepare(&mut a, &s, 0);
                prepare(&mut b, &s, 1);
                let path = root(dir.path(), &s)
                    .join("payload")
                    .join(a.provider.ownership().leaf);
                assert!(a.publish(&AtomicBool::new(true)).is_err());
                assert!(path.exists());
                drop(b);
                let [a, b] = parts(dir.path(), &s);
                a.discard(3, &AtomicBool::new(false)).unwrap();
                assert!(!path.exists());
                drop(b);
            },
        );

        isolated("reset_fence_survives_before_and_after_truncation", || {
            for truncate in [false, true] {
                let dir = directory();
                let s = selected();
                let [mut a, mut b] = parts(dir.path(), &s);
                prepare(&mut b, &s, 1);
                {
                    let mut session = Session::new(&s, 0, 1, &mut a.provider).unwrap();
                    let mut prepared = session
                        .prepare(&s, &mut a.file, &AtomicBool::new(false))
                        .unwrap();
                    crate::setup::internal_cache::tests::start_prefix(&mut prepared, b"EEEE");
                    use sha2::{Digest, Sha256};
                    prepared
                        .observer
                        .boundary(
                            prepared.id,
                            prepared.file,
                            Candidate {
                                length: 0,
                                digest: Sha256::digest([]).into(),
                                kind: Kind::Reset,
                            },
                        )
                        .unwrap();
                    if truncate {
                        prepared.file.set_len(0).unwrap();
                        prepared.file.sync_all().unwrap();
                    }
                }
                drop(a);
                drop(b);
                let [mut a, b] = parts(dir.path(), &s);
                let mut session = Session::new(&s, 0, 2, &mut a.provider).unwrap();
                let prepared = session
                    .prepare(&s, &mut a.file, &AtomicBool::new(false))
                    .unwrap();
                assert_eq!(prepared.committed(), 0);
                assert_eq!(prepared.file.metadata().unwrap().len(), 0);
                drop(prepared);
                drop(session);
                drop(b);
            }
        });
    }
    /// Failed post-scan preparation and new session revocation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn linux_failed_preparation_revokes_success_without_publishing() {
        isolated(
            "failed_post_scan_preparation_cannot_publish_or_retire_rejected_records",
            || {
                use sha2::{Digest, Sha256};
                fn snapshot(path: &std::path::Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
                    let mut entries: Vec<_> = fs::read_dir(path)
                        .unwrap()
                        .map(|e| {
                            let e = e.unwrap();
                            (e.file_name(), fs::read(e.path()).unwrap())
                        })
                        .collect();
                    entries.sort();
                    entries
                }
                // Each case has a genuine authenticated full component. Native bootstrap
                // and record decoding succeed; only common history/prefix validation
                // distinguishes the independently valid control from the two failures.
                for case in 0..3 {
                    let dir = directory();
                    let s = selected();
                    let [mut a, mut b] = parts(dir.path(), &s);
                    prepare(&mut b, &s, 1);
                    finish(&mut a, &s, 0);
                    let leaf = a.provider.ownership().leaf;
                    drop(a);
                    drop(b);
                    let journal = root(dir.path(), &s).join("journal-0");
                    if case > 0 {
                        let old = journal.join(if case == 1 {
                            "record-0000000002"
                        } else {
                            "record-0000000001"
                        });
                        let mut bytes = fs::read(&old).unwrap();
                        if case == 1 {
                            bytes[layout::GENERATION..layout::EPOCH]
                                .copy_from_slice(&3u64.to_le_bytes());
                        } else {
                            bytes[layout::DIGEST] ^= 1;
                        }
                        let checksum = Sha256::digest(&bytes[..bytes.len() - 32]);
                        let end = bytes.len();
                        bytes[end - 32..].copy_from_slice(&checksum);
                        fs::write(&old, bytes).unwrap();
                        if case == 1 {
                            fs::rename(old, journal.join("record-0000000003")).unwrap();
                        }
                    }
                    let partial = root(dir.path(), &s).join("payload").join(leaf);
                    let final_path = root(dir.path(), &s)
                        .join("payload")
                        .join(s.bundle().component(0).unwrap().filename);
                    let before_payload = fs::read(&partial).unwrap();
                    let before_records = snapshot(&journal);
                    let [mut a, b] = parts(dir.path(), &s);
                    {
                        let mut session = Session::new(&s, 0, 3, &mut a.provider).unwrap();
                        let result = session.prepare(&s, &mut a.file, &AtomicBool::new(false));
                        if case == 0 {
                            assert!(result.is_ok());
                        } else {
                            let error = match result {
                                Err(e) => e,
                                Ok(_) => panic!("rejected history prepared"),
                            };
                            assert!(error.contains(if case == 1 {
                                "Conflicting checkpoint history"
                            } else {
                                "Checkpoint prefix digest mismatch"
                            }));
                        }
                    }
                    if case == 0 {
                        let mut final_file = a.publish(&AtomicBool::new(false)).unwrap();
                        assert!(!final_file.has_retained_residue());
                        final_file.verify(&AtomicBool::new(false)).unwrap();
                        assert!(!partial.exists());
                        assert_eq!(fs::read(&final_path).unwrap(), before_payload);
                        assert!(snapshot(&journal).is_empty());
                    } else {
                        assert!(a.provider.retain(&[]).is_err());
                        assert!(a.publish(&AtomicBool::new(false)).is_err());
                        assert!(!final_path.exists());
                        assert_eq!(fs::read(&partial).unwrap(), before_payload);
                        assert_eq!(snapshot(&journal), before_records);
                    }
                    drop(b);
                }
            },
        );

        isolated(
            "failed_new_session_revokes_prior_native_preparation_success",
            || {
                let dir = directory();
                let s = selected();
                let [mut a, mut b] = parts(dir.path(), &s);
                prepare(&mut b, &s, 1);
                finish(&mut a, &s, 0);
                let partial = root(dir.path(), &s)
                    .join("payload")
                    .join(a.provider.ownership().leaf);
                assert!(Session::new(&s, 0, 0, &mut a.provider).is_err());
                assert!(a.publish(&AtomicBool::new(false)).is_err());
                assert_eq!(fs::read(partial).unwrap(), vec![b'E'; 4096]);
                assert!(
                    !root(dir.path(), &s)
                        .join("payload")
                        .join(s.bundle().component(0).unwrap().filename)
                        .exists()
                );
                drop(b);
            },
        );
    }
}

#[cfg(target_os = "linux")]
thread_local! { static FAIL_AT: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) }; }
#[cfg(target_os = "linux")]
thread_local! { static CANCEL_AT: std::cell::RefCell<Option<(&'static str, std::sync::Arc<std::sync::atomic::AtomicBool>)>> = const { std::cell::RefCell::new(None) }; }
#[cfg(target_os = "linux")]
pub(super) fn boundary(stage: &'static str) -> Result<(), String> {
    CANCEL_AT.with(|slot| {
        if let Some((at, cancel)) = slot.borrow().as_ref() {
            if *at == stage {
                cancel.store(true, std::sync::atomic::Ordering::Release);
            }
        }
    });
    FAIL_AT.with(|slot| {
        if *slot.borrow() == Some(stage) {
            Err(format!(
                "injected ENOSPC at {stage}: {}",
                std::io::Error::from_raw_os_error(libc::ENOSPC)
            ))
        } else {
            Ok(())
        }
    })
}
