use super::*;
#[test]
fn private_aces_refuse_every_untrusted_grant_and_unknown_mask() {
    assert!(!allowed_ace(false, 1, true));
    assert!(!allowed_ace(true, 0x8000_0000, true));
    assert!(allowed_ace(true, 0x001f_01ff, true));
}
#[test]
fn ancestors_allow_read_and_add_directory_but_not_replacement() {
    assert!(allowed_ace(false, 0x0012_0089, false));
    assert!(allowed_ace(false, 4, false));
    for mask in [2, 16, 64, 256, 0x10000, 0x40000, 0x80000] {
        assert!(!allowed_ace(false, mask, false), "{mask:x}");
    }
}
#[test]
fn raw_unicode_and_surrogates_are_lossless_but_aliases_refuse() {
    let mut raw: Vec<_> = "C:\\用户\\".encode_utf16().collect();
    raw.push(0xd800);
    let parsed = windows_directory_path(raw).unwrap();
    assert_eq!(parsed.names[1], [0xd800]);
    for path in [
        "C:relative",
        "C:/bad",
        "C:\\a\\..\\b",
        "C:\\a ",
        "C:\\COM¹",
        "C:\\con.txt",
        "C:\\a~1",
        "C:\\a:b",
    ] {
        assert!(
            windows_directory_path(path.encode_utf16()).is_err(),
            "{path}"
        );
    }
    assert!(windows_directory_path(format!("C:\\{}", "a".repeat(255)).encode_utf16()).is_ok());
    assert!(windows_directory_path(format!("C:\\{}", "a".repeat(256)).encode_utf16()).is_err());
    assert!(windows_directory_path(std::iter::repeat_n(65, 4097)).is_err());
}

#[test]
fn cache_suffix_must_still_fit_the_verifier_path_boundary() {
    let okay = format!("C:\\{}", vec!["x"; 61].join("\\"));
    assert!(windows_cache_path(okay.encode_utf16()).is_ok());
    let too_many = format!("C:\\{}", vec!["x"; 62].join("\\"));
    assert!(windows_cache_path(too_many.encode_utf16()).is_err());
    let near_limit = format!("C:\\{}", vec!["x".repeat(250); 16].join("\\"));
    assert!(windows_directory_path(near_limit.encode_utf16()).is_ok());
    assert!(windows_cache_path(near_limit.encode_utf16()).is_err());
}

// Native Windows fixtures: UNRUN until explicit platform qualification authorization.
#[cfg(windows)]
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
    use std::{fs, io::Write, os::windows::ffi::OsStringExt, sync::atomic::AtomicBool};
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
    fn start_prefix(prepared: &mut crate::setup::internal_cache::PreparedPrefix<'_>, bytes: &[u8]) {
        prepared
            .observer
            .begin_attempt(prepared.id, prepared.file)
            .unwrap();
        prepared.file.write_all(bytes).unwrap();
        prepared.hashes.written(bytes);
        prepared
            .hashes
            .verify_tail(prepared.file, &AtomicBool::new(false))
            .unwrap();
        let ack = prepared
            .observer
            .boundary(
                prepared.id,
                prepared.file,
                Candidate {
                    length: prepared.hashes.length,
                    digest: prepared.hashes.digest(),
                    kind: Kind::Prefix,
                },
            )
            .unwrap();
        prepared.id = ack.id;
        prepared.committed = prepared.hashes.length;
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
    fn failed_post_scan_preparation_cannot_publish_or_retire_rejected_records() {
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
                    bytes[147..155].copy_from_slice(&3u64.to_le_bytes());
                } else {
                    bytes[179] ^= 1;
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
    }
    #[test]
    fn failed_new_session_revokes_prior_native_preparation_success() {
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
    }
    #[test]
    fn fresh_settlement_never_grants_final_publication_or_reusable_freshness() {
        let dir = directory();
        let s = selected();
        let [mut a, mut b] = parts(dir.path(), &s);
        a.file.write_all(&vec![b'E'; 4096]).unwrap();
        a.file.sync_all().unwrap();
        assert!(a.publish(&AtomicBool::new(false)).is_err());
        assert!(
            crate::setup::internal_cache::initialize_fresh(&s, 1, &mut b.provider, &mut b.file)
                .is_err()
        );
        assert!(b.provider.retain(&[]).is_err());
    }
    #[test]
    fn cancellation_during_zero_record_settles_owned_creation_then_remains_terminal() {
        use std::sync::{Arc, atomic::Ordering};
        let dir = directory();
        let s = selected();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        super::HOOK.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |stage| {
                if stage == "record-created" {
                    flag.store(true, Ordering::Release);
                }
            }))
        });
        let result = fixture_acquire(dir.path(), &s)
            .unwrap()
            .bootstrap(&s, &cancel);
        super::HOOK.with(|h| *h.borrow_mut() = None);
        assert!(result.is_err());
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(
            fs::read_dir(root(dir.path(), &s).join("payload"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            fs::read_dir(root(dir.path(), &s).join("journal-0"))
                .unwrap()
                .count(),
            1
        );
        let [mut a, mut b] = parts(dir.path(), &s);
        prepare(&mut a, &s, 0);
        prepare(&mut b, &s, 1);
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
                "setup::internal_cache::windows::tests::native::lock_probe",
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
    fn raw_unicode_cache_ancestry_survives_close_and_resume() {
        let dir = directory();
        let path = dir
            .path()
            .join(std::ffi::OsString::from_wide(&[0x7528, 0x6237, 0xd800]));
        fs::create_dir(&path).unwrap();
        let s = selected();
        let [mut a, mut b] = parts(&path, &s);
        prepare(&mut b, &s, 1);
        {
            let mut session = Session::new(&s, 0, 1, &mut a.provider).unwrap();
            let mut prepared = session
                .prepare(&s, &mut a.file, &AtomicBool::new(false))
                .unwrap();
            start_prefix(&mut prepared, b"EEEE");
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
    fn record_flush_faults_leave_bounded_residue_and_never_claim_success() {
        for stage in ["record-created", "record-written", "record-flushed"] {
            let dir = directory();
            let s = selected();
            super::FAULT.with(|v| *v.borrow_mut() = Some(stage));
            assert!(
                fixture_acquire(dir.path(), &s)
                    .unwrap()
                    .bootstrap(&s, &AtomicBool::new(false))
                    .is_err()
            );
            super::FAULT.with(|v| *v.borrow_mut() = None);
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
        for stage in ["final-renamed", "writer-closed", "final-verified"] {
            let dir = directory();
            let s = selected();
            let [mut a, mut b] = parts(dir.path(), &s);
            prepare(&mut b, &s, 1);
            finish(&mut a, &s, 0);
            super::FAULT.with(|v| *v.borrow_mut() = Some(stage));
            assert!(a.publish(&AtomicBool::new(false)).is_err());
            super::FAULT.with(|v| *v.borrow_mut() = None);
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
                start_prefix(&mut prepared, b"EEEE");
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
        super::FAULT.with(|v| *v.borrow_mut() = Some("partial-flushed"));
        assert!(
            fixture_acquire(dir.path(), &s)
                .unwrap()
                .bootstrap(&s, &AtomicBool::new(false))
                .is_err()
        );
        super::FAULT.with(|v| *v.borrow_mut() = None);
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
            super::super::native::fixture_file(&final_path, &bytes).unwrap();
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
    fn writable_and_final_handles_enforce_sharing_and_parent_pins() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = directory();
        let s = selected();
        let [mut a, mut b] = parts(dir.path(), &s);
        prepare(&mut b, &s, 1);
        finish(&mut a, &s, 0);
        let path = root(dir.path(), &s)
            .join("payload")
            .join(a.provider.ownership().leaf);
        assert!(
            fs::OpenOptions::new()
                .write(true)
                .share_mode(7)
                .open(&path)
                .is_err()
        );
        assert!(fs::rename(root(dir.path(), &s), dir.path().join("moved")).is_err());
        let final_file = a.publish(&AtomicBool::new(false)).unwrap();
        let final_path = root(dir.path(), &s)
            .join("payload")
            .join(s.bundle().component(0).unwrap().filename);
        assert!(
            fs::OpenOptions::new()
                .write(true)
                .share_mode(7)
                .open(&final_path)
                .is_err()
        );
        assert!(fs::remove_file(&final_path).is_err());
        drop(final_file);
        drop(b);
    }
    #[test]
    fn hardlinks_reparse_and_unknown_residue_are_refused_untouched() {
        use std::os::windows::fs::symlink_dir;
        for case in 0..3 {
            let dir = directory();
            let s = selected();
            let [mut a, mut b] = parts(dir.path(), &s);
            prepare(&mut a, &s, 0);
            prepare(&mut b, &s, 1);
            let path = root(dir.path(), &s)
                .join("payload")
                .join(a.provider.ownership().leaf);
            drop(a);
            drop(b);
            match case {
                0 => fs::hard_link(&path, dir.path().join("alias")).unwrap(),
                1 => {
                    let payload = root(dir.path(), &s).join("payload");
                    fs::rename(&payload, dir.path().join("saved")).unwrap();
                    symlink_dir(dir.path().join("saved"), &payload).unwrap();
                }
                _ => fs::write(root(dir.path(), &s).join("payload/unknown"), b"keep").unwrap(),
            }
            let result = fixture_acquire(dir.path(), &s)
                .and_then(|r| r.bootstrap(&s, &AtomicBool::new(false)));
            assert!(result.is_err());
            assert!(path.exists());
            if case == 2 {
                assert_eq!(
                    fs::read(root(dir.path(), &s).join("payload/unknown")).unwrap(),
                    b"keep"
                );
            }
        }
    }
    #[test]
    fn existing_permissive_or_unprotected_acl_refuses_without_repair() {
        for protected in [false, true] {
            let dir = directory();
            let s = selected();
            let [mut a, mut b] = parts(dir.path(), &s);
            prepare(&mut a, &s, 0);
            prepare(&mut b, &s, 1);
            drop(a);
            drop(b);
            let path = root(dir.path(), &s).join("payload");
            super::super::native::fixture_acl(&path, protected).unwrap();
            assert!(fixture_acquire(dir.path(), &s).is_err());
            assert!(fixture_acquire(dir.path(), &s).is_err());
            assert_eq!(fs::read_dir(path).unwrap().count(), 2);
        }
    }
    #[test]
    fn journal_delete_sharing_failure_keeps_checkpoint_residue() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = directory();
        let s = selected();
        let [mut a, mut b] = parts(dir.path(), &s);
        prepare(&mut a, &s, 0);
        prepare(&mut b, &s, 1);
        let record = root(dir.path(), &s).join("journal-0/record-0000000001");
        let blocker = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&record)
            .unwrap();
        let error = a.discard(3, &AtomicBool::new(false)).unwrap_err();
        assert!(error.contains("discard incomplete"));
        assert!(record.exists());
        drop(blocker);
        drop(b);
    }
    #[test]
    fn close_reopen_substitution_with_authentic_bytes_is_rejected() {
        let dir = directory();
        let s = selected();
        let [mut a, mut b] = parts(dir.path(), &s);
        prepare(&mut b, &s, 1);
        finish(&mut a, &s, 0);
        let path = root(dir.path(), &s)
            .join("payload")
            .join(s.bundle().component(0).unwrap().filename);
        let captured = path.clone();
        super::HOOK.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |stage| {
                if stage == "writer-closed" {
                    fs::rename(&captured, captured.with_extension("saved")).unwrap();
                    super::super::native::fixture_file(&captured, &vec![b'E'; 4096]).unwrap();
                }
            }))
        });
        let result = a.publish(&AtomicBool::new(false));
        super::HOOK.with(|h| *h.borrow_mut() = None);
        assert!(matches!(result, Err(e) if e.contains("close/reopen identity/security changed")));
        assert_eq!(fs::read(&path).unwrap(), vec![b'E'; 4096]);
        assert_eq!(
            fs::read_dir(root(dir.path(), &s).join("journal-0"))
                .unwrap()
                .count(),
            2
        );
        drop(b);
    }
    #[test]
    #[ignore = "Requires an externally provisioned quota-limited NTFS/ReFS cache; never fill a host volume"]
    fn disk_full_does_not_acknowledge_or_delete_residue() {
        // Provision enough space for acquisition, but less than the 4096-byte payload.
        let path = std::env::var_os("QOS_WINDOWS_QUOTA_CACHE").expect("qualified quota fixture");
        let s = selected();
        let [mut a, mut b] = parts(std::path::Path::new(&path), &s);
        prepare(&mut a, &s, 0);
        prepare(&mut b, &s, 1);
        let bytes = vec![b'E'; 4096];
        let result = a.file.write_all(&bytes).and_then(|_| a.file.sync_all());
        assert!(
            result.is_err(),
            "fixture must exhaust its own quota, not the host volume"
        );
        assert!(a.publish(&AtomicBool::new(false)).is_err());
        drop(b);
        let [mut a, b] = parts(std::path::Path::new(&path), &s);
        prepare(&mut a, &s, 0);
        assert_eq!(a.file.metadata().unwrap().len(), 0);
        drop(b);
    }
}

#[cfg(windows)]
thread_local! { static HOOK: std::cell::RefCell<Option<Box<dyn FnMut(&str)>>> = const { std::cell::RefCell::new(None) }; static FAULT: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) }; }
#[cfg(windows)]
pub(super) fn boundary(stage: &'static str) -> Result<(), String> {
    HOOK.with(|h| {
        if let Some(f) = h.borrow_mut().as_mut() {
            f(stage);
        }
    });
    if FAULT.with(|f| *f.borrow() == Some(stage)) {
        Err(format!("Injected boundary: {stage}"))
    } else {
        Ok(())
    }
}
