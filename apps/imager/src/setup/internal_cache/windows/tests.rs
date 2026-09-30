use super::*;
/// Private ACEs, ancestor rights, raw Unicode and path boundary.
#[test]
fn windows_aces_and_names_are_strict_lossless_and_bounded() {
    // Phase `private_aces_refuse_every_untrusted_grant_and_unknown_mask`.
    {
        assert!(!allowed_ace(false, 1, true));
        assert!(!allowed_ace(true, 0x8000_0000, true));
        assert!(allowed_ace(true, 0x001f_01ff, true));
    }

    // Phase `ancestors_allow_read_and_add_directory_but_not_replacement`.
    {
        assert!(allowed_ace(false, 0x0012_0089, false));
        assert!(allowed_ace(false, 4, false));
        for mask in [2, 16, 64, 256, 0x10000, 0x40000, 0x80000] {
            assert!(!allowed_ace(false, mask, false), "{mask:x}");
        }
    }

    // Phase `raw_unicode_and_surrogates_are_lossless_but_aliases_refuse`.
    {
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

    // Phase `cache_suffix_must_still_fit_the_verifier_path_boundary`.
    {
        let okay = format!("C:\\{}", vec!["x"; 61].join("\\"));
        assert!(windows_cache_path(okay.encode_utf16()).is_ok());
        let too_many = format!("C:\\{}", vec!["x"; 62].join("\\"));
        assert!(windows_cache_path(too_many.encode_utf16()).is_err());
        let near_limit = format!("C:\\{}", vec!["x".repeat(250); 16].join("\\"));
        assert!(windows_directory_path(near_limit.encode_utf16()).is_ok());
        assert!(windows_cache_path(near_limit.encode_utf16()).is_err());
    }
}
// Native Windows fixtures: UNRUN until explicit platform qualification authorization.
#[cfg(windows)]
mod native {
    use super::super::{native::fixture_acquire, *};
    use crate::setup::internal_cache::layout;
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
            "https://code.tilo.so/releases/manifest.json",
        );
        SelectedManifest::from_bytes(&release, bytes).unwrap()
    }
    fn directory() -> tempfile::TempDir {
        // No /tmp fallback: native fixtures need the qualified current-user
        // cache filesystem and safe ancestry, just like the production provider.
        tempfile::tempdir_in(tilo_paths::cache_dir().unwrap()).unwrap()
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
    /// Failed preparation, session revocation, fresh settlement and zero-record cancellation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn windows_preparation_and_settlement_never_grant_publication() {
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

        isolated(
            "fresh_settlement_never_grants_final_publication_or_reusable_freshness",
            || {
                let dir = directory();
                let s = selected();
                let [mut a, mut b] = parts(dir.path(), &s);
                a.file.write_all(&vec![b'E'; 4096]).unwrap();
                a.file.sync_all().unwrap();
                assert!(a.publish(&AtomicBool::new(false)).is_err());
                assert!(
                    crate::setup::internal_cache::initialize_fresh(
                        &s,
                        1,
                        &mut b.provider,
                        &mut b.file
                    )
                    .is_err()
                );
                assert!(b.provider.retain(&[]).is_err());
            },
        );

        isolated(
            "cancellation_during_zero_record_settles_owned_creation_then_remains_terminal",
            || {
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
            },
        );
    }
    /// Process lock, raw Unicode ancestry, handle sharing and reparse refusal.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn windows_locks_handles_and_ancestry_refuse_substitution() {
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
                        "setup::internal_cache::windows::tests::native::lock_probe",
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

        isolated(
            "raw_unicode_cache_ancestry_survives_close_and_resume",
            || {
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
            },
        );

        isolated(
            "writable_and_final_handles_enforce_sharing_and_parent_pins",
            || {
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
            },
        );

        isolated(
            "hardlinks_reparse_and_unknown_residue_are_refused_untouched",
            || {
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
                        _ => fs::write(root(dir.path(), &s).join("payload/unknown"), b"keep")
                            .unwrap(),
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
    /// Flush faults, crash boundaries, reset fences and zero-record residue.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn windows_record_faults_and_crashes_recover_without_claiming_success() {
        isolated(
            "record_flush_faults_leave_bounded_residue_and_never_claim_success",
            || {
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
            },
        );

        isolated(
            "final_crash_boundaries_recover_final_before_stale_partial_records",
            || {
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
        });

        isolated(
            "creation_before_zero_record_is_unknown_residue_even_when_empty",
            || {
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
            },
        );
    }
    /// Cancellation, visitor errors, collisions, sharing failure and reopen substitution.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn windows_cancellation_collisions_and_reopen_keep_residue_honest() {
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
            },
        );

        isolated(
            "journal_delete_sharing_failure_keeps_checkpoint_residue",
            || {
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
            },
        );

        isolated(
            "close_reopen_substitution_with_authentic_bytes_is_rejected",
            || {
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
                            super::super::native::fixture_file(&captured, &vec![b'E'; 4096])
                                .unwrap();
                        }
                    }))
                });
                let result = a.publish(&AtomicBool::new(false));
                super::HOOK.with(|h| *h.borrow_mut() = None);
                assert!(
                    matches!(result, Err(e) if e.contains("close/reopen identity/security changed"))
                );
                assert_eq!(fs::read(&path).unwrap(), vec![b'E'; 4096]);
                assert_eq!(
                    fs::read_dir(root(dir.path(), &s).join("journal-0"))
                        .unwrap()
                        .count(),
                    2
                );
                drop(b);
            },
        );
    }
    // These independent SDK observations deliberately do not call production
    // security(), trusted(), descriptor(), current_user(), or allowed_ace().
    // They inspect owner/group/DACL (not privileged SACL state).
    mod security_observation {
        use std::{
            ffi::c_void,
            fs::{File, OpenOptions},
            mem::size_of,
            os::windows::{
                fs::OpenOptionsExt,
                io::{AsRawHandle, FromRawHandle, OwnedHandle},
            },
            path::Path,
            ptr,
        };
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{Authorization::*, *},
            Storage::FileSystem::*,
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        };
        struct Local(*mut c_void);
        impl Drop for Local {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    unsafe {
                        LocalFree(self.0);
                    }
                }
            }
        }
        fn success(value: i32) {
            assert_ne!(value, 0, "{}", std::io::Error::last_os_error());
        }
        // SID comes from a live OS-owned descriptor/ACE or the bounded token
        // allocation. Check its fixed header and complete subauthority extent.
        unsafe fn sid_text(sid: PSID, start: usize, end: usize) -> String {
            let p = sid as usize;
            assert!(p >= start && p.checked_add(8).is_some_and(|n| n <= end));
            let count = unsafe { *sid.cast::<u8>().add(1) } as usize;
            assert!(count <= 15 && p.checked_add(8 + 4 * count).is_some_and(|n| n <= end));
            success(unsafe { IsValidSid(sid) });
            let mut output = ptr::null_mut();
            success(unsafe { ConvertSidToStringSidW(sid, &mut output) });
            let _allocation = Local(output.cast());
            let mut n = 0;
            while n < 256 && unsafe { *output.add(n) } != 0 {
                n += 1;
            }
            assert!(n < 256);
            String::from_utf16(unsafe { std::slice::from_raw_parts(output, n) }).unwrap()
        }
        pub(super) fn user() -> String {
            let mut raw = ptr::null_mut();
            success(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) });
            assert!(!raw.is_null());
            let token = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut size = 0;
            unsafe {
                GetTokenInformation(
                    token.as_raw_handle(),
                    TokenUser,
                    ptr::null_mut(),
                    0,
                    &mut size,
                );
            }
            assert!((size_of::<TOKEN_USER>()..=65536).contains(&(size as usize)));
            let capacity = size;
            let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
            success(unsafe {
                GetTokenInformation(
                    token.as_raw_handle(),
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    capacity,
                    &mut size,
                )
            });
            assert!(size <= capacity && size as usize >= size_of::<TOKEN_USER>());
            let start = buffer.as_ptr() as usize;
            unsafe {
                sid_text(
                    (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid,
                    start,
                    start + size as usize,
                )
            }
        }
        #[derive(Debug, PartialEq, Eq)]
        pub(super) struct Ace {
            pub kind: u8,
            pub flags: u8,
            pub mask: u32,
            pub sid: String,
        }
        #[derive(Debug, PartialEq, Eq)]
        pub(super) struct Facts {
            pub identity: (u64, [u8; 16]),
            pub owner: String,
            pub protected: bool,
            pub aces: Vec<Ace>,
            pub descriptor: String,
        }
        fn open(path: &Path, access: u32) -> File {
            OpenOptions::new()
                .access_mode(access)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
                .unwrap()
        }
        pub(super) fn inspect(path: &Path) -> Facts {
            let file = open(path, READ_CONTROL | FILE_READ_ATTRIBUTES);
            let mut id = FILE_ID_INFO::default();
            success(unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileIdInfo,
                    (&raw mut id).cast(),
                    size_of::<FILE_ID_INFO>() as u32,
                )
            });
            let mut owner = ptr::null_mut();
            let mut dacl: *mut ACL = ptr::null_mut();
            let mut sd = ptr::null_mut();
            let flags =
                OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
            assert_eq!(
                unsafe {
                    GetSecurityInfo(
                        file.as_raw_handle(),
                        SE_FILE_OBJECT,
                        flags,
                        &mut owner,
                        ptr::null_mut(),
                        &mut dacl,
                        ptr::null_mut(),
                        &mut sd,
                    )
                },
                0
            );
            let _allocation = Local(sd);
            assert!(
                !sd.is_null() && !dacl.is_null(),
                "fixture requires a non-null DACL"
            );
            success(unsafe { IsValidSecurityDescriptor(sd) });
            let len = unsafe { GetSecurityDescriptorLength(sd) } as usize;
            assert!((20..=65536).contains(&len));
            let start = sd as usize;
            let end = start.checked_add(len).unwrap();
            let owner = unsafe { sid_text(owner, start, end) };
            let dp = dacl as usize;
            assert!(dp >= start && dp.checked_add(size_of::<ACL>()).is_some_and(|n| n <= end));
            let size = unsafe { (*dacl).AclSize } as usize;
            assert!(size >= size_of::<ACL>() && dp.checked_add(size).is_some_and(|n| n <= end));
            success(unsafe { IsValidAcl(dacl) });
            assert!(unsafe { (*dacl).AceCount } <= 128);
            let mut control = 0;
            let mut revision = 0;
            success(unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) });
            let mut aces = Vec::new();
            for i in 0..unsafe { (*dacl).AceCount } {
                let mut ace: *mut c_void = ptr::null_mut();
                success(unsafe { GetAce(dacl, i as u32, &mut ace) });
                let ap = ace as usize;
                assert!(ap >= dp && ap.checked_add(8).is_some_and(|n| n <= dp + size));
                let header = unsafe { &*ace.cast::<ACE_HEADER>() };
                let ace_end = ap.checked_add(header.AceSize as usize).unwrap();
                assert!(header.AceSize >= 16 && ace_end <= dp + size);
                assert!(
                    matches!(header.AceType, 0 | 1),
                    "fixture uses ordinary allow/deny ACEs"
                );
                let sid = unsafe { sid_text(ace.cast::<u8>().add(8).cast(), ap + 8, ace_end) };
                aces.push(Ace {
                    kind: header.AceType,
                    flags: header.AceFlags,
                    mask: unsafe { (*ace.cast::<ACCESS_ALLOWED_ACE>()).Mask },
                    sid,
                });
            }
            let mut text = ptr::null_mut();
            let mut units = 0;
            success(unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    sd, 1, flags, &mut text, &mut units,
                )
            });
            let _text = Local(text.cast());
            assert!(!text.is_null() && (1..=65536).contains(&units));
            let raw = unsafe { std::slice::from_raw_parts(text, units as usize) };
            assert_eq!(raw.last(), Some(&0));
            Facts {
                identity: (id.VolumeSerialNumber, id.FileId.Identifier),
                owner,
                protected: control & SE_DACL_PROTECTED != 0,
                aces,
                descriptor: String::from_utf16(&raw[..raw.len() - 1]).unwrap(),
            }
        }
        pub(super) fn parent_acl(path: &Path, extra: &str) {
            // Test-only setup on the test-owned parent, BEFORE creating the
            // inheritance control or production descendants. Never changes owner.
            let file = open(path, WRITE_DAC | READ_CONTROL);
            let user = user();
            let text: Vec<u16> =
                format!("D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA){extra}\0")
                    .encode_utf16()
                    .collect();
            let mut sd = ptr::null_mut();
            success(unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    1,
                    &mut sd,
                    ptr::null_mut(),
                )
            });
            let _allocation = Local(sd);
            let mut dacl = ptr::null_mut();
            let mut present = 0;
            let mut defaulted = 0;
            success(unsafe {
                GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted)
            });
            assert_ne!(present, 0);
            assert!(!dacl.is_null());
            assert_eq!(
                unsafe {
                    SetSecurityInfo(
                        file.as_raw_handle(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        dacl,
                        ptr::null_mut(),
                    )
                },
                0
            );
        }
        pub(super) fn trusted_only(facts: &Facts, user: &str) {
            // Literal independent expected principals/rights; no production predicate.
            assert_eq!(facts.aces.len(), 3);
            for sid in [user, "S-1-5-18", "S-1-5-32-544"] {
                let matches: Vec<_> = facts.aces.iter().filter(|a| a.sid == sid).collect();
                assert_eq!(matches.len(), 1, "{facts:?}");
                let ace = matches[0];
                assert_eq!(ace.kind, 0);
                assert_eq!(ace.mask, FILE_ALL_ACCESS);
                assert_eq!(u32::from(ace.flags) & INHERIT_ONLY_ACE, 0);
            }
        }
        pub(super) fn private(path: &Path, user: &str) {
            let facts = inspect(path);
            assert_eq!(facts.owner, user, "{path:?}");
            assert!(facts.protected, "{path:?}");
            trusted_only(&facts, user);
        }
    }
    // Descriptor, full native identity, object type, names and file bytes all
    // survive a refusal. Ignore access timestamps, which inspection can update.
    fn security_tree(
        path: &std::path::Path,
    ) -> Vec<(
        std::path::PathBuf,
        security_observation::Facts,
        bool,
        Vec<u8>,
    )> {
        fn walk(
            base: &std::path::Path,
            path: &std::path::Path,
            out: &mut Vec<(
                std::path::PathBuf,
                security_observation::Facts,
                bool,
                Vec<u8>,
            )>,
        ) {
            assert!(out.len() < 32);
            let metadata = fs::symlink_metadata(path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            assert!(metadata.is_dir() || metadata.is_file());
            let bytes = if metadata.is_file() {
                assert!(metadata.len() <= 4096);
                fs::read(path).unwrap()
            } else {
                Vec::new()
            };
            out.push((
                path.strip_prefix(base).unwrap().to_owned(),
                security_observation::inspect(path),
                metadata.is_dir(),
                bytes,
            ));
            if metadata.is_dir() {
                let mut children: Vec<_> = fs::read_dir(path)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .collect();
                assert!(children.len() <= 16);
                children.sort();
                for child in children {
                    walk(base, &child, out);
                }
            }
        }
        let mut result = Vec::new();
        walk(path, path, &mut result);
        result
    }
    fn inspect_private_descendants(
        cache: &std::path::Path,
        s: &SelectedManifest,
        parts: &[Partial<'_>; 2],
        user: &str,
    ) {
        let manifest = root(cache, s);
        for path in [
            cache.join("tilo-internal-install"),
            manifest.clone(),
            manifest.join("payload"),
            manifest.join("journal-0"),
            manifest.join("journal-1"),
            manifest.join("ownership.lock"),
        ] {
            security_observation::private(&path, user);
        }
        for (i, part) in parts.iter().enumerate() {
            security_observation::private(
                &manifest
                    .join("payload")
                    .join(part.provider.ownership().leaf),
                user,
            );
            security_observation::private(
                &manifest.join(format!("journal-{i}/record-0000000001")),
                user,
            );
        }
    }
    /// Protected creation, ancestry replacement grants and permissive ACLs.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn windows_cache_acls_are_protected_and_never_repaired() {
        isolated(
            "protected_creation_excludes_effective_inheritance_from_preconfigured_parent",
            || {
                use windows_sys::Win32::{
                    Security::{CONTAINER_INHERIT_ACE, INHERIT_ONLY_ACE, OBJECT_INHERIT_ACE},
                    Storage::FileSystem::FILE_ALL_ACCESS,
                };
                let dir = directory();
                let s = selected();
                let user = security_observation::user();
                security_observation::parent_acl(dir.path(), "(A;OICIIO;FA;;;WD)");
                let before = security_observation::inspect(dir.path());
                assert_eq!(before.owner, user);
                assert!(before.aces.iter().any(|a| a.sid == "S-1-1-0"
                    && a.kind == 0
                    && a.mask == FILE_ALL_ACCESS
                    && u32::from(a.flags)
                        & (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE | INHERIT_ONLY_ACE)
                        == OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE | INHERIT_ONLY_ACE));
                // An ordinary child positively demonstrates that this parent really
                // transmits an effective Everyone grant; it is outside the cache namespace.
                let ordinary = dir.path().join("inheritance-control");
                fs::create_dir(&ordinary).unwrap();
                fs::write(ordinary.join("sentinel"), b"inherited").unwrap();
                for path in [&ordinary, &ordinary.join("sentinel")] {
                    let inherited = security_observation::inspect(path);
                    assert!(!inherited.protected);
                    assert!(inherited.aces.iter().any(|a| a.kind == 0
                        && a.sid == "S-1-1-0"
                        && a.mask == FILE_ALL_ACCESS
                        && u32::from(a.flags) & INHERIT_ONLY_ACE == 0));
                }
                let parts = parts(dir.path(), &s); // Must reach real exclusive private creation.
                inspect_private_descendants(dir.path(), &s, &parts, &user);
                assert_eq!(security_observation::inspect(dir.path()), before);
            },
        );

        isolated(
            "effective_ancestry_replacement_grant_refuses_without_descriptor_or_object_changes",
            || {
                let dir = directory();
                let s = selected();
                fs::write(dir.path().join("sentinel"), b"unchanged").unwrap();
                security_observation::parent_acl(dir.path(), "(A;;0x40;;;WD)");
                let facts = security_observation::inspect(dir.path());
                assert_eq!(facts.owner, security_observation::user());
                assert!(
                    facts.aces.iter().any(|a| a.kind == 0
                        && a.sid == "S-1-1-0"
                        && a.mask == 0x40
                        && a.flags == 0)
                );
                let before = security_tree(dir.path());
                let error = match fixture_acquire(dir.path(), &s) {
                    Err(e) => e,
                    Ok(_) => panic!("replacement authority accepted"),
                };
                assert!(error.contains("Unsafe cache ACL rights"), "{error}");
                assert_eq!(security_tree(dir.path()), before);
                assert!(!dir.path().join("tilo-internal-install").exists());
            },
        );

        isolated(
            "existing_permissive_or_unprotected_acl_refuses_without_repair",
            || {
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
                    let before = security_tree(&root(dir.path(), &s));
                    for _ in 0..2 {
                        let error = match fixture_acquire(dir.path(), &s) {
                            Err(e) => e,
                            Ok(_) => panic!("unsafe existing private ACL accepted"),
                        };
                        assert!(
                            error.contains(if protected {
                                "Private ACL contains foreign principal"
                            } else {
                                "Private DACL not protected"
                            }),
                            "{error}"
                        );
                        assert_eq!(security_tree(&root(dir.path(), &s)), before);
                    }
                    assert_eq!(fs::read_dir(path).unwrap().count(), 2);
                }
            },
        );
    }
    #[test]
    #[ignore = "UNRUN: externally provision a dedicated SYSTEM-owned local ancestry with protected current-user/SYSTEM/Administrators full-access DACL; no account or privilege changes in fixture"]
    fn externally_provisioned_system_owned_ancestry_is_accepted() {
        let path = std::path::PathBuf::from(
            std::env::var_os("TILO_WINDOWS_SYSTEM_ANCESTOR")
                .expect("externally provisioned SYSTEM-owned ancestry"),
        );
        let user = security_observation::user();
        let before = security_observation::inspect(&path);
        assert_ne!(user, "S-1-5-18", "run as an ordinary current user");
        assert_eq!(before.owner, "S-1-5-18");
        assert!(before.protected);
        security_observation::trusted_only(&before, &user);
        let dir = tempfile::tempdir_in(&path).unwrap();
        let s = selected();
        let parts = parts(dir.path(), &s);
        inspect_private_descendants(dir.path(), &s, &parts, &user);
        assert_eq!(security_observation::inspect(&path), before);
    }
    #[test]
    #[ignore = "UNRUN: externally provision a dedicated foreign-owner local ancestry, trusted-only full-access DACL and sole sentinel file; provide expected foreign SID; never change owner/accounts/privileges in fixture"]
    fn externally_provisioned_untrusted_owner_refuses_without_repair() {
        let path = std::path::PathBuf::from(
            std::env::var_os("TILO_WINDOWS_FOREIGN_OWNER_ANCESTOR")
                .expect("externally provisioned foreign-owner ancestry"),
        );
        let expected = std::env::var("TILO_WINDOWS_FOREIGN_OWNER_SID")
            .expect("expected externally provisioned owner SID");
        let user = security_observation::user();
        let facts = security_observation::inspect(&path);
        assert_eq!(facts.owner, expected);
        assert!(
            ![
                user.as_str(),
                "S-1-5-18",
                "S-1-5-32-544",
                "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
            ]
            .contains(&expected.as_str())
        );
        assert!(facts.protected);
        security_observation::trusted_only(&facts, &user);
        assert_eq!(fs::read_dir(&path).unwrap().count(), 1);
        assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"owner-fixture");
        let before = security_tree(&path);
        let s = selected();
        let error = match fixture_acquire(&path, &s) {
            Err(e) => e,
            Ok(_) => panic!("foreign owner accepted"),
        };
        assert!(error.contains("Cache owner refusal"), "{error}");
        assert_eq!(security_tree(&path), before);
    }
    #[test]
    #[ignore = "Requires an externally provisioned quota-limited NTFS/ReFS cache; never fill a host volume"]
    fn disk_full_does_not_acknowledge_or_delete_residue() {
        // Provision enough space for acquisition, but less than the 4096-byte payload.
        let path = std::env::var_os("TILO_WINDOWS_QUOTA_CACHE").expect("qualified quota fixture");
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

    /// Runs one merged phase on a fresh thread so the thread-local fault and
    /// cancellation hooks start clean, exactly as when the phase was its own
    /// test; a failing phase re-raises its original panic payload.
    fn isolated(phase: &str, body: impl FnOnce() + Send) {
        std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .name(phase.to_owned())
                .spawn_scoped(scope, body)
                .expect("spawn isolated test phase");
            if let Err(panic) = handle.join() {
                std::panic::resume_unwind(panic);
            }
        });
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
