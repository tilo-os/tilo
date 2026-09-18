use super::*;

#[test]
fn raw_ancestry_is_bounded_lossless_and_refuses_normalization() {
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

// These fixtures exercise actual Linux syscalls. They are source-checked only
// under the current runtime holds; host tests above make no native claim.
#[cfg(target_os = "linux")]
mod native {
    use super::super::{native::fixture_acquire, *};
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
            "https://code.podesta.ai/releases/manifest.json",
        );
        SelectedManifest::from_bytes(&release, bytes).unwrap()
    }
    fn directory() -> tempfile::TempDir {
        // No /tmp fallback: native fixtures need the qualified current-user
        // cache filesystem and safe ancestry, just like the production provider.
        tempfile::tempdir_in(dirs::cache_dir().unwrap()).unwrap()
    }
    fn root(path: &std::path::Path, s: &SelectedManifest) -> std::path::PathBuf {
        path.join("qos-internal-install").join(s.manifest_sha256())
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
    #[test]
    fn independent_process_lock_is_retained_by_either_component_and_final() {
        let dir = directory();
        let s = selected();
        let [mut a, mut b] = parts(dir.path(), &s);
        prepare(&mut b, &s, 1);
        finish(&mut a, &s, 0);
        let final_file = a.publish(&AtomicBool::new(false)).unwrap();
        assert!(!final_file.has_retained_residue());
        drop(b);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "setup::internal_cache::linux::tests::native::lock_probe",
                "--ignored",
            ])
            .env("QOS_CACHE_LOCK_PROBE", dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        drop(final_file);
        assert!(fixture_acquire(dir.path(), &s).is_ok());
    }
    #[test]
    #[ignore = "subprocess fixture invoked with a held parent lease"]
    fn lock_probe() {
        let dir = std::env::var_os("QOS_CACHE_LOCK_PROBE").expect("parent fixture path");
        let result = fixture_acquire(std::path::Path::new(&dir), &selected());
        assert!(matches!(result,Err(e) if e=="Cache Busy"));
    }
    #[test]
    fn non_utf8_cache_ancestry_survives_close_and_resume() {
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
    }
    #[test]
    fn unsafe_ancestor_hardlink_substitution_and_unknown_residue_refuse_untouched() {
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
                    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777)).unwrap();
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
                    fs::write(root(dir.path(), &s).join("journal-0/torn"), b"unknown").unwrap();
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
    }
    #[test]
    fn no_replace_collision_authenticates_winner_and_preserves_invalid_leaf() {
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
    }
    #[test]
    fn record_durability_faults_leave_bounded_residue_and_never_claim_success() {
        for stage in ["record-created", "record-written", "record-durable"] {
            let dir = directory();
            let s = selected();
            let [mut a, mut b] = parts(dir.path(), &s);
            prepare(&mut b, &s, 1);
            super::FAIL_AT.with(|v| *v.borrow_mut() = Some(stage));
            let mut session = Session::new(&s, 0, 1, &mut a.provider).unwrap();
            assert!(
                session
                    .prepare(&s, &mut a.file, &AtomicBool::new(false))
                    .is_err()
            );
            drop(session);
            super::FAIL_AT.with(|v| *v.borrow_mut() = None);
            drop(a);
            drop(b);
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
    }
    #[test]
    fn final_crash_boundaries_recover_final_before_stale_partial_records() {
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
    }
    #[test]
    fn cancellation_preserves_owned_prefix_and_discard_is_explicit() {
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
    }
    #[test]
    fn safe_effective_acl_and_inherited_default_acl_are_supported() {
        use std::os::fd::AsRawFd;
        for mask in [0u16, 2] {
            let dir = directory();
            let file = std::fs::File::open(dir.path()).unwrap();
            let mut acl = 2u32.to_le_bytes().to_vec();
            let uid = unsafe { libc::geteuid() };
            for (tag, perm, id) in [
                (1u16, 7u16, u32::MAX),
                (2, 7, uid + 1),
                (4, 0, u32::MAX),
                (16, mask, u32::MAX),
                (32, 0, u32::MAX),
            ] {
                acl.extend_from_slice(&tag.to_le_bytes());
                acl.extend_from_slice(&perm.to_le_bytes());
                acl.extend_from_slice(&id.to_le_bytes());
            }
            assert_eq!(
                unsafe {
                    libc::fsetxattr(
                        file.as_raw_fd(),
                        c"system.posix_acl_access".as_ptr(),
                        acl.as_ptr().cast(),
                        acl.len(),
                        0,
                    )
                },
                0
            );
            let s = selected();
            let result = fixture_acquire(dir.path(), &s);
            assert_eq!(result.is_ok(), mask == 0);
            drop(result);
            if mask == 0 {
                assert_eq!(
                    unsafe {
                        libc::fsetxattr(
                            file.as_raw_fd(),
                            c"system.posix_acl_default".as_ptr(),
                            acl.as_ptr().cast(),
                            acl.len(),
                            0,
                        )
                    },
                    0
                );
                let components = fixture_acquire(dir.path(), &s)
                    .unwrap()
                    .bootstrap(&s, &AtomicBool::new(false))
                    .unwrap();
                drop(components);
            }
        }
    }
    #[test]
    fn reset_fence_survives_before_and_after_truncation() {
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
    }
    #[test]
    fn creation_before_zero_record_is_unknown_residue_even_when_empty() {
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
    }
    #[test]
    fn changed_held_ancestor_and_excessive_entries_refuse_without_cleanup() {
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
    }
    #[test]
    fn fresh_scan_stops_immediately_on_visitor_error() {
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
    }
}

#[test]
fn effective_acl_mask_permits_safe_named_grants_and_refuses_effective_access() {
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

#[cfg(target_os = "linux")]
thread_local! { static FAIL_AT: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) }; }
#[cfg(target_os = "linux")]
pub(super) fn boundary(stage: &'static str) -> Result<(), String> {
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
