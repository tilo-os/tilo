use super::*;
use std::os::unix::{
    ffi::OsStringExt,
    fs::{MetadataExt, symlink},
};

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let home = base.join("home");
    let target = base.join("imported-home");
    let private = base.join("private");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    (temp, home, target, private.join("home.journal"))
}

#[test]
fn whole_home_worker_transfers_hidden_data_links_modes_and_preserves_originals() {
    let (_temp, home, target, journal) = fixture();
    fs::create_dir(home.join(".config")).unwrap();
    fs::create_dir(home.join(".ssh")).unwrap();
    fs::set_permissions(home.join(".ssh"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(home.join(".config/preferences"), b"all my preferences").unwrap();
    fs::write(home.join(".ssh/id_ed25519"), b"private fixture credential").unwrap();
    fs::set_permissions(
        home.join(".ssh/id_ed25519"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(home.join("run-me"), b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(home.join("run-me"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::hard_link(home.join("run-me"), home.join("second-name")).unwrap();
    symlink(".config/preferences", home.join("relative-link")).unwrap();
    symlink("/outside/does-not-exist", home.join("absolute-link")).unwrap();
    let native_name = std::ffi::OsString::from_vec(vec![b'a', 0xff]);
    fs::write(home.join(&native_name), b"non UTF-8 names survive").unwrap();
    let source_inode = fs::metadata(home.join("run-me")).unwrap().ino();
    let plan = review(&home, &target, &journal).unwrap();
    assert_eq!(plan.summary.links, 2);
    assert_eq!(plan.summary.files, 5);
    let mut worker = plan.start().unwrap();
    let outcome = worker.finish();
    let WorkerOutcome::Complete(report) = outcome else {
        panic!("Whole-home import failed: {outcome:?}");
    };
    assert!(report.complete(), "{report:?}");
    assert_eq!(
        fs::read(target.join(".config/preferences")).unwrap(),
        b"all my preferences"
    );
    assert_eq!(
        fs::read(target.join(".ssh/id_ed25519")).unwrap(),
        b"private fixture credential"
    );
    assert_eq!(
        fs::metadata(target.join("run-me"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(target.join(".ssh/id_ed25519"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(target.join("run-me")).unwrap().ino(),
        fs::metadata(target.join("second-name")).unwrap().ino()
    );
    assert_eq!(
        fs::read_link(target.join("relative-link")).unwrap(),
        Path::new(".config/preferences")
    );
    assert_eq!(
        fs::read_link(target.join("absolute-link")).unwrap(),
        Path::new("/outside/does-not-exist")
    );
    assert_eq!(
        fs::read(target.join(&native_name)).unwrap(),
        b"non UTF-8 names survive"
    );
    assert_eq!(
        fs::metadata(home.join("run-me")).unwrap().ino(),
        source_inode
    );
    assert_eq!(
        fs::read(home.join(".ssh/id_ed25519")).unwrap(),
        b"private fixture credential"
    );
    // The home-specific retry uses its finalized receipts, not generic 0600
    // receipts. It verifies an unchanged completed import and every hardlink.
    let WorkerOutcome::Complete(_) = plan.retry().unwrap().finish() else {
        panic!("Finalized home retry must revalidate");
    };
    fs::write(target.join("run-me"), b"user modified imported output").unwrap();
    let WorkerOutcome::Incomplete(report) = plan.retry().unwrap().finish() else {
        panic!("Changed retained output must be incomplete");
    };
    assert!(!report.failed.is_empty());
    assert_eq!(
        fs::read(target.join("run-me")).unwrap(),
        b"user modified imported output"
    );
}

#[test]
fn home_finalization_recovers_after_a_durable_interruption() {
    let (_temp, home, target, journal) = fixture();
    fs::write(home.join("executable"), b"fixture bytes").unwrap();
    fs::set_permissions(home.join("executable"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("executable", home.join("link")).unwrap();
    let plan = review(&home, &target, &journal).unwrap();
    fidelity::interrupt_after_checkpoint();
    let error = plan
        .transfer(false, &AtomicBool::new(false), |_| {})
        .unwrap_err();
    assert!(error.contains("durable home checkpoint"));
    let reopened = HomeReview::recover(&home, &target, &journal).unwrap();
    let report = reopened
        .transfer(true, &AtomicBool::new(false), |_| {})
        .unwrap();
    assert!(report.complete(), "{report:?}");
    assert_eq!(
        fs::metadata(target.join("executable"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        fs::read_link(target.join("link")).unwrap(),
        Path::new("executable")
    );
    assert_eq!(fs::read(home.join("executable")).unwrap(), b"fixture bytes");
}

#[test]
fn home_preserves_user_attributes_and_refuses_changed_retained_metadata() {
    use std::{ffi::CString, os::fd::AsRawFd};
    let (_temp, home, target, journal) = fixture();
    fs::write(home.join("document"), b"bytes").unwrap();
    let name = CString::new("user.fixture").unwrap();
    let value = b"custom home metadata";
    let file = File::open(home.join("document")).unwrap();
    // SAFETY: fixture descriptor and bounded live name/value buffers.
    assert_eq!(
        unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        },
        0
    );
    let plan = review(&home, &target, &journal).unwrap();
    assert!(matches!(
        plan.start().unwrap().finish(),
        WorkerOutcome::Complete(_)
    ));
    let file = File::open(target.join("document")).unwrap();
    let mut actual = vec![0_u8; value.len()];
    // SAFETY: fixture descriptor and bounded live writable output buffer.
    assert_eq!(
        unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                actual.as_mut_ptr().cast(),
                actual.len(),
            )
        },
        value.len() as isize
    );
    assert_eq!(actual, value);
    let replacement = b"changed";
    // SAFETY: fixture descriptor and bounded live name/value buffers.
    assert_eq!(
        unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                name.as_ptr(),
                replacement.as_ptr().cast(),
                replacement.len(),
                0,
            )
        },
        0
    );
    let WorkerOutcome::Incomplete(report) = plan.retry().unwrap().finish() else {
        panic!("Changed metadata must require review");
    };
    assert!(
        report
            .failed
            .iter()
            .any(|(_, error)| error.contains("extended attribute changed"))
    );
    // A saved review is not authority for another supplied destination.
    assert!(HomeReview::recover(&home, &target.with_extension("different"), &journal).is_err());
}

#[test]
fn cancellation_before_byte_copy_has_a_durable_recoverable_review() {
    let (_temp, home, target, journal) = fixture();
    fs::write(home.join("document"), b"source stays unchanged").unwrap();
    let plan = review(&home, &target, &journal).unwrap();
    let report = plan
        .transfer(false, &AtomicBool::new(true), |_| {})
        .unwrap();
    assert!(report.cancelled);
    assert!(!target.exists());
    assert!(!journal.exists());
    let recovered = HomeReview::recover(&home, &target, &journal).unwrap();
    assert!(matches!(
        recovered.retry().unwrap().finish(),
        WorkerOutcome::Complete(_)
    ));
    assert_eq!(
        fs::read(target.join("document")).unwrap(),
        b"source stays unchanged"
    );
}

#[test]
fn changing_home_during_copy_never_claims_a_completed_snapshot() {
    let (_temp, home, target, journal) = fixture();
    fs::write(home.join("document"), b"reviewed bytes").unwrap();
    let plan = review(&home, &target, &journal).unwrap();
    let mut changed = false;
    let result = plan.transfer(false, &AtomicBool::new(false), |_| {
        if !changed {
            changed = true;
            fs::write(home.join("document"), b"new user edit during copy").unwrap();
        }
    });
    match result {
        Err(error) => assert!(error.contains("changed after review"), "{error}"),
        Ok(report) => assert!(!report.complete(), "{report:?}"),
    }
    assert_eq!(
        fs::read(home.join("document")).unwrap(),
        b"new user edit during copy"
    );
    assert!(!journal.with_extension("home-finalization").exists());
}

#[test]
fn replaced_directory_or_link_is_never_followed_during_recovery() {
    let (_temp, home, target, journal) = fixture();
    fs::create_dir(home.join(".config")).unwrap();
    fs::write(home.join(".config/settings"), b"preferences").unwrap();
    symlink(".config/settings", home.join("alias")).unwrap();
    let plan = review(&home, &target, &journal).unwrap();
    assert!(matches!(
        plan.start().unwrap().finish(),
        WorkerOutcome::Complete(_)
    ));
    let outside = home.parent().unwrap().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("settings"), b"leave these bytes alone").unwrap();
    fs::rename(target.join(".config"), target.join("retained-config")).unwrap();
    symlink(&outside, target.join(".config")).unwrap();
    let WorkerOutcome::Incomplete(report) = plan.retry().unwrap().finish() else {
        panic!("Replacement directory must require review");
    };
    assert!(!report.failed.is_empty());
    assert_eq!(
        fs::read(outside.join("settings")).unwrap(),
        b"leave these bytes alone"
    );
    assert_eq!(
        fs::read(target.join("retained-config/settings")).unwrap(),
        b"preferences"
    );
}

#[test]
fn sparse_home_files_keep_their_bytes_without_allocating_all_zero_extents() {
    use std::io::{Seek, SeekFrom, Write};
    let (_temp, home, target, journal) = fixture();
    let mut source = File::create(home.join("sparse-image")).unwrap();
    source.write_all(b"image header").unwrap();
    source.set_len(4 * 1024 * 1024).unwrap();
    source.seek(SeekFrom::End(-4)).unwrap();
    source.write_all(b"tail").unwrap();
    source.sync_all().unwrap();
    let plan = review(&home, &target, &journal).unwrap();
    assert!(matches!(
        plan.start().unwrap().finish(),
        WorkerOutcome::Complete(_)
    ));
    assert_eq!(
        fs::read(target.join("sparse-image")).unwrap(),
        fs::read(home.join("sparse-image")).unwrap()
    );
    let metadata = fs::metadata(target.join("sparse-image")).unwrap();
    assert!(
        metadata.blocks() * 512 < metadata.len() / 2,
        "Sparse copy allocated its full logical size"
    );
}

#[test]
fn home_review_refuses_overlap_and_source_changes_before_creating_output() {
    let (_temp, home, target, journal) = fixture();
    fs::write(home.join("document"), b"original").unwrap();
    assert!(review(&home, &home.join("recursive-copy"), &journal).is_err());
    assert!(!home.join("recursive-copy").exists());
    let plan = review(&home, &target, &journal).unwrap();
    fs::write(home.join("document"), b"changed after consent").unwrap();
    assert!(matches!(
        plan.start().unwrap().finish(),
        WorkerOutcome::Failed(_)
    ));
    assert!(!target.exists());
    assert!(!journal.exists());
    let link = home.parent().unwrap().join("home-alias");
    symlink(&home, &link).unwrap();
    assert!(review(&link, &target, &journal).is_err());
}

#[test]
fn home_special_entries_are_reported_without_following_or_blocking() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let (_temp, home, target, journal) = fixture();
    fs::write(home.join("document"), b"ordinary bytes").unwrap();
    let pipe = CString::new(home.join("pipe").as_os_str().as_bytes()).unwrap();
    // SAFETY: valid NUL-terminated temporary fixture path, no live user path.
    assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
    let plan = review(&home, &target, &journal).unwrap();
    assert_eq!(plan.summary.special_files, 1);
    let WorkerOutcome::Incomplete(report) = plan.start().unwrap().finish() else {
        panic!("Special entries require a truthful incomplete report");
    };
    assert_eq!(report.skipped.len(), 1);
    assert!(!target.join("pipe").exists());
    assert_eq!(
        fs::read(target.join("document")).unwrap(),
        b"ordinary bytes"
    );
    assert!(fs::symlink_metadata(home.join("pipe")).is_ok());
}

#[test]
fn runtime_sockets_have_durable_omission_receipts_without_losing_adjacent_keys() {
    use std::os::unix::net::UnixListener;
    let (_temp, home, target, journal) = fixture();
    fs::create_dir(home.join(".gnupg")).unwrap();
    fs::set_permissions(home.join(".gnupg"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(home.join(".gnupg/private-key"), b"private key fixture").unwrap();
    fs::set_permissions(
        home.join(".gnupg/private-key"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let socket = home.join(".gnupg/S.gpg-agent");
    let _listener = UnixListener::bind(&socket).unwrap();
    let source_inode = fs::symlink_metadata(&socket).unwrap().ino();
    let plan = review(&home, &target, &journal).unwrap();
    assert_eq!(plan.summary.runtime_sockets, 1);
    assert_eq!(plan.summary.special_files, 0);
    let WorkerOutcome::Complete(report) = plan.start().unwrap().finish() else {
        panic!("Runtime socket endpoints are disclosed omissions, not missing persistent data");
    };
    assert!(report.complete());
    assert!(report.skipped.is_empty());
    assert!(report.metadata_limitations.iter().any(
        |message| message.contains(".gnupg/S.gpg-agent") && message.contains("runtime socket")
    ));
    assert_eq!(
        fs::read(target.join(".gnupg/private-key")).unwrap(),
        b"private key fixture"
    );
    assert!(!target.join(".gnupg/S.gpg-agent").exists());
    assert_eq!(fs::symlink_metadata(&socket).unwrap().ino(), source_inode);
    assert_eq!(
        fs::read(home.join(".gnupg/private-key")).unwrap(),
        b"private key fixture"
    );
    let log = fs::read_to_string(journal.with_extension("home-finalization")).unwrap();
    let mut lines = log.lines();
    let manifest: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let socket_index = manifest["entries"]
        .as_array()
        .unwrap()
        .iter()
        .position(|entry| entry["kind"]["kind"] == "RuntimeSocket")
        .unwrap();
    assert!(lines.any(|line| {
        let receipt: serde_json::Value = serde_json::from_str(line).unwrap();
        receipt["index"].as_u64() == Some(socket_index as u64) && receipt["destination"].is_null()
    }));
    let WorkerOutcome::Complete(resumed) = plan.retry().unwrap().finish() else {
        panic!("A durable socket omission must revalidate during retry");
    };
    assert!(
        resumed
            .metadata_limitations
            .iter()
            .any(|message| message.contains(".gnupg/S.gpg-agent"))
    );
    fs::write(
        target.join(".gnupg/S.gpg-agent"),
        b"do not overwrite this target",
    )
    .unwrap();
    let WorkerOutcome::Incomplete(conflict) = plan.retry().unwrap().finish() else {
        panic!("An omission receipt must not waive a new target conflict");
    };
    assert!(
        conflict
            .failed
            .iter()
            .any(|(_, error)| error.contains("runtime socket omission conflicts"))
    );
    assert_eq!(
        fs::read(target.join(".gnupg/S.gpg-agent")).unwrap(),
        b"do not overwrite this target"
    );
}

#[test]
fn app_phase_proof_allows_new_deployments_but_preserves_original_objects_and_userdata() {
    let (_temp, home, target, journal) = fixture();
    fs::create_dir_all(home.join(".local/share/flatpak/repo/objects")).unwrap();
    fs::create_dir_all(home.join(".var/app/org.example.User/data")).unwrap();
    fs::write(
        home.join(".local/share/flatpak/repo/objects/original"),
        b"original user object",
    )
    .unwrap();
    fs::write(
        home.join(".var/app/org.example.User/data/session"),
        b"original user session",
    )
    .unwrap();
    fs::write(home.join("personal"), b"original personal document").unwrap();
    let reviewed = review(&home, &target, &journal).unwrap();
    assert!(matches!(
        reviewed.start().unwrap().finish(),
        WorkerOutcome::Complete(_)
    ));
    fs::create_dir_all(target.join(".local/share/flatpak/app/org.example.System")).unwrap();
    fs::write(
        target.join(".local/share/flatpak/app/org.example.System/deployment"),
        b"new system deployment",
    )
    .unwrap();
    assert!(reviewed.verify_after_applications().unwrap().complete());
    let recovered = HomeReview::recover(&home, &target, &journal).unwrap();
    assert!(recovered.verify_after_applications().unwrap().complete());
    fs::write(
        target.join(".local/share/flatpak/repo/objects/original"),
        b"overwritten user object",
    )
    .unwrap();
    assert!(recovered.verify_after_applications().is_err());
    assert_eq!(
        fs::read(home.join(".local/share/flatpak/repo/objects/original")).unwrap(),
        b"original user object"
    );
    assert_eq!(
        fs::read(target.join(".var/app/org.example.User/data/session")).unwrap(),
        b"original user session"
    );
}
#[test]
fn app_phase_proof_refuses_unreviewed_additions_outside_installation() {
    let (_temp, home, target, journal) = fixture();
    fs::write(home.join("personal"), b"document").unwrap();
    let reviewed = review(&home, &target, &journal).unwrap();
    assert!(matches!(
        reviewed.start().unwrap().finish(),
        WorkerOutcome::Complete(_)
    ));
    fs::create_dir_all(target.join(".local/share/flatpak/repo")).unwrap();
    assert!(reviewed.verify_after_applications().unwrap().complete());
    fs::write(target.join(".local/share/unreviewed"), b"unexpected").unwrap();
    assert!(reviewed.verify_after_applications().is_err());
    assert_eq!(
        fs::read(target.join(".local/share/unreviewed")).unwrap(),
        b"unexpected"
    );
}

#[test]
fn home_preserves_posix_acl_defaults_and_refuses_changed_permissions() {
    let (_temp, home, target, journal) = fixture();
    let source = home.join("shared");
    fs::create_dir(&source).unwrap();
    use crate::portable_transfer::acl::{policy, set_test_acl};
    let permissions = b"user::rwx\ngroup::r-x\nmask::r-x\nother::---";
    set_test_acl(&source, false, permissions).unwrap();
    set_test_acl(&source, true, permissions).unwrap();
    fs::write(source.join("document"), b"ACL fixture").unwrap();
    let plan = review(&home, &target, &journal).unwrap();
    assert!(matches!(
        plan.start().unwrap().finish(),
        WorkerOutcome::Complete(_)
    ));
    let acl = |path: &Path| policy(&fs::File::open(path).unwrap()).unwrap();
    assert!(acl(&source).iter().all(Option::is_some));
    assert!(acl(&source.join("document"))[0].is_some());
    assert_eq!(acl(&source), acl(&target.join("shared")));
    assert_eq!(
        acl(&source.join("document")),
        acl(&target.join("shared/document"))
    );
    set_test_acl(
        &target.join("shared/document"),
        false,
        b"user::rw-\ngroup::r-x\nmask::---\nother::---",
    )
    .unwrap();
    assert!(matches!(
        plan.retry().unwrap().finish(),
        WorkerOutcome::Incomplete(_)
    ));
    assert_eq!(fs::read(source.join("document")).unwrap(), b"ACL fixture");
}
