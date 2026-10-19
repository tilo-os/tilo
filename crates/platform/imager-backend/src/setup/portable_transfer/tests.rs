use super::*;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
};
const PASSWORD: &[u8] = b"a long destination password";
fn limits() -> Limits {
    Limits {
        plaintext_bytes: 8 * 1024 * 1024,
        entries: 1024,
        metadata_bytes: 1024 * 1024,
    }
}
#[test]
fn sealed_frames_authenticate_complete_stream_and_purpose() {
    let cancel = AtomicBool::new(false);
    let plain = vec![71; codec::CHUNK + 5];
    let mut sealer = codec::Sealer::new(
        Vec::new(),
        PASSWORD,
        Purpose::PreparedSetup,
        8 * 1024 * 1024,
        &cancel,
    )
    .unwrap();
    sealer.write_all(&plain).unwrap();
    let (sealed, _) = sealer.finish(0).unwrap();
    let mut reader =
        codec::Opener::new(sealed.as_slice(), PASSWORD, 8 * 1024 * 1024, &cancel).unwrap();
    assert_eq!(reader.purpose, Purpose::PreparedSetup);
    let mut actual = Vec::new();
    reader.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, plain);
    assert!(reader.footer.is_some());
    for broken in [
        sealed[..sealed.len() - 1].to_vec(),
        {
            let mut bytes = sealed.clone();
            bytes.push(0);
            bytes
        },
        {
            let mut bytes = sealed.clone();
            let last = bytes.len() - 20;
            bytes[last] ^= 1;
            bytes
        },
    ] {
        let mut reader =
            codec::Opener::new(broken.as_slice(), PASSWORD, 8 * 1024 * 1024, &cancel).unwrap();
        assert!(reader.read_to_end(&mut Vec::new()).is_err());
    }
    assert!(
        codec::Opener::new(
            sealed.as_slice(),
            b"wrong password",
            8 * 1024 * 1024,
            &cancel
        )
        .is_err()
    );
}
#[test]
fn portable_home_roundtrip_preserves_data_and_never_stages_plaintext() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    fs::create_dir(source.join(".var")).unwrap();
    fs::create_dir(source.join(".var/app")).unwrap();
    fs::write(
        source.join(".var/app/account"),
        b"a very private credential sentinel",
    )
    .unwrap();
    fs::write(source.join("script"), b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(source.join("script"), fs::Permissions::from_mode(0o750)).unwrap();
    use std::os::{fd::AsRawFd, unix::ffi::OsStringExt};
    let source_file = fs::File::open(source.join("script")).unwrap();
    let xattr_name = std::ffi::CString::new("user.tilo-fixture").unwrap();
    let xattr_value = b"preserved";
    // SAFETY: valid descriptor/name and a readable immutable byte slice.
    assert_eq!(
        unsafe {
            libc::fsetxattr(
                source_file.as_raw_fd(),
                xattr_name.as_ptr(),
                xattr_value.as_ptr().cast(),
                xattr_value.len(),
                0,
            )
        },
        0
    );
    crate::portable_transfer::acl::fixture::modify(
        &source.join("script"),
        "u::rwx,g::r-x,m::r-x,o::---",
    );
    crate::portable_transfer::acl::fixture::modify(
        &source.join(".var/app"),
        "d:u::rwx,d:g::r-x,d:m::r-x,d:o::---",
    );
    let native_name = std::ffi::OsString::from_vec(vec![b'n', 0xff]);
    fs::write(source.join(&native_name), b"native filename").unwrap();
    fs::hard_link(source.join("script"), source.join("hard-script")).unwrap();
    std::os::unix::fs::symlink("../../outside", source.join("literal-link")).unwrap();
    let _runtime_socket =
        std::os::unix::net::UnixListener::bind(source.join("agent.sock")).unwrap();
    crate::portable_transfer::acl::fixture::modify(
        &source,
        "u::rwx,g::r-x,m::r-x,o::r-x,d:u::rwx,d:g::r-x,d:m::r-x,d:o::---",
    );
    let package = root.join("migration.sealed");
    let cancel = AtomicBool::new(false);
    let review = ExportReview::review(&source, &package, Purpose::Home, limits()).unwrap();
    assert!(review.summary().unwrap().bytes > 0);
    assert_eq!(review.summary().unwrap().runtime_sockets, 1);
    let info = review
        .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    assert!(info.sealed_bytes > 0);
    assert_eq!(info.runtime_sockets, 1);
    assert!(
        !fs::read(&package)
            .unwrap()
            .windows(b"a very private credential sentinel".len())
            .any(|v| v == b"a very private credential sentinel")
    );
    assert!(
        ImportReview::review(
            &package,
            PASSWORD,
            Purpose::PreparedSetup,
            limits(),
            &cancel
        )
        .is_err()
    );
    let imported =
        ImportReview::review(&package, PASSWORD, Purpose::Home, limits(), &cancel).unwrap();
    assert_eq!(imported.info().purpose, Purpose::Home);
    assert_eq!(imported.info().runtime_sockets, 1);
    let tree = imported
        .extract(
            Zeroizing::new(PASSWORD.to_vec()),
            &root.join("quarantine"),
            &cancel,
        )
        .unwrap();
    for relative in ["script", ".var/app", ".var/app/account"] {
        assert_eq!(
            crate::portable_transfer::acl::fixture::state(&source.join(relative)),
            crate::portable_transfer::acl::fixture::state(&tree.root.join(relative))
        );
    }
    assert_eq!(
        fs::metadata(&tree.root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let default_acl = |path: &Path| crate::portable_transfer::acl::fixture::state(path).2;
    assert_eq!(default_acl(&source), default_acl(&tree.root));
    assert_eq!(tree.purpose, Purpose::Home);
    assert!(tree.info.entries >= 6);
    assert_eq!(tree.info.runtime_sockets, 1);
    assert!(!tree.root.join("agent.sock").exists());
    assert!(source.join("agent.sock").exists());
    assert_eq!(
        fs::read(tree.root.join(".var/app/account")).unwrap(),
        b"a very private credential sentinel"
    );
    assert_eq!(
        fs::read_link(tree.root.join("literal-link")).unwrap(),
        Path::new("../../outside")
    );
    assert_eq!(
        fs::metadata(tree.root.join("script"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(
        fs::read(tree.root.join(&native_name)).unwrap(),
        b"native filename"
    );
    let target_file = fs::File::open(tree.root.join("script")).unwrap();
    let mut actual_xattr = [0; 32];
    // SAFETY: valid descriptor/name and a writable bounded array.
    let length = unsafe {
        libc::fgetxattr(
            target_file.as_raw_fd(),
            xattr_name.as_ptr(),
            actual_xattr.as_mut_ptr().cast(),
            actual_xattr.len(),
        )
    };
    assert_eq!(length, xattr_value.len() as isize);
    assert_eq!(&actual_xattr[..length as usize], xattr_value);
    assert_eq!(
        fs::read(source.join(".var/app/account")).unwrap(),
        b"a very private credential sentinel"
    );
}

fn package_of_archive(archive: &[u8]) -> Vec<u8> {
    let cancel = AtomicBool::new(false);
    let mut sealer = codec::Sealer::new(
        Vec::new(),
        PASSWORD,
        Purpose::Home,
        8 * 1024 * 1024,
        &cancel,
    )
    .unwrap();
    sealer.write_all(archive).unwrap();
    sealer.finish(0).unwrap().0
}
fn header(kind: tar::EntryType, size: u64) -> tar::Header {
    let mut header = tar::Header::new_ustar();
    header.set_entry_type(kind);
    header.set_size(size);
    header.set_mode(0o700);
    header.set_mtime(0);
    header.set_cksum();
    header
}
#[test]
fn authenticated_archive_cannot_write_through_a_literal_link_or_repeat_an_entry() {
    for duplicate in [false, true] {
        let mut tar = tar::Builder::new(Vec::new());
        tar.append_data(&mut header(tar::EntryType::Directory, 0), ".", io::empty())
            .unwrap();
        if duplicate {
            tar.append_data(
                &mut header(tar::EntryType::Regular, 1),
                "file",
                b"a".as_slice(),
            )
            .unwrap();
            tar.append_data(
                &mut header(tar::EntryType::Regular, 1),
                "file",
                b"b".as_slice(),
            )
            .unwrap();
        } else {
            let mut link = header(tar::EntryType::Symlink, 0);
            link.set_link_name("../../outside").unwrap();
            link.set_cksum();
            tar.append_data(&mut link, "link", io::empty()).unwrap();
            tar.append_data(
                &mut header(tar::EntryType::Regular, 1),
                "link/file",
                b"a".as_slice(),
            )
            .unwrap();
        }
        let bytes = package_of_archive(&tar.into_inner().unwrap());
        let cancel = AtomicBool::new(false);
        let reader = codec::Opener::new(
            bytes.as_slice(),
            PASSWORD,
            limits().plaintext_bytes,
            &cancel,
        )
        .unwrap();
        assert!(archive::scan(reader, limits()).is_err());
    }
}
#[test]
fn cancelled_or_changed_source_does_not_publish_a_complete_package() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"reviewed").unwrap();
    let package = root.join("package");
    let review = ExportReview::review(&source, &package, Purpose::Home, limits()).unwrap();
    fs::write(source.join("file"), b"changed").unwrap();
    assert!(
        review
            .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
            .is_err()
    );
    assert!(!package.exists());
    let review = ExportReview::review(&source, &package, Purpose::Home, limits()).unwrap();
    assert!(
        review
            .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(true))
            .is_err()
    );
    assert!(!package.exists());
}
#[test]
fn mid_export_cancellation_reaps_actual_tar_removes_ciphertext_and_retries() {
    use std::{cell::Cell, rc::Rc, sync::atomic::Ordering};

    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    let contents = vec![73; 4 * codec::CHUNK];
    fs::write(source.join(".application-data"), &contents).unwrap();
    let package = root.join("package.sealed");
    let review = ExportReview::review(&source, &package, Purpose::Home, limits()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let pid = Rc::new(Cell::new(None));
    let observed_pid = pid.clone();
    let observed_cancel = cancel.clone();
    let observed_package = package.clone();
    let mut bytes_read = 0usize;
    let observer = process::StreamObserver::install(move |event| match event {
        process::StreamEvent::Spawned(child) => observed_pid.set(Some(child)),
        process::StreamEvent::Read(count) => {
            bytes_read = bytes_read.checked_add(count).unwrap();
            if bytes_read > 2 * codec::CHUNK {
                // Cancellation is triggered by actual archive IO after a sealed
                // frame has reached the real exclusive destination file.
                assert!(fs::metadata(&observed_package).unwrap().len() > codec::CHUNK as u64);
                observed_cancel.store(true, Ordering::Release);
            }
        }
        process::StreamEvent::Written(_) => {}
    });
    assert!(
        review
            .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
            .is_err()
    );
    drop(observer);
    assert!(cancel.load(Ordering::Acquire));
    let child = i32::try_from(pid.get().unwrap()).unwrap();
    // SAFETY: signal zero only probes the actual owned child PID recorded at spawn.
    assert_eq!(unsafe { libc::kill(child, 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    assert!(!package.exists());
    assert_eq!(
        fs::read(source.join(".application-data")).unwrap(),
        contents
    );
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);

    cancel.store(false, Ordering::Release);
    let mut progress = Vec::new();
    let info = review
        .export_reporting(Zeroizing::new(PASSWORD.to_vec()), &cancel, &mut |p| {
            progress.push(p);
            Ok(())
        })
        .unwrap();
    assert_archive_progress(&progress, &info);
    assert_ciphertext_progress(&package, &root, &cancel, &info);
    assert!(info.sealed_bytes > codec::CHUNK as u64);
    assert_mid_import_cancelled(&package, &root, &cancel);
    let imported = ImportReview::review(&package, PASSWORD, Purpose::Home, limits(), &cancel)
        .unwrap()
        .extract(
            Zeroizing::new(PASSWORD.to_vec()),
            &root.join("quarantine"),
            &cancel,
        )
        .unwrap();
    assert_eq!(
        fs::read(imported.root.join(".application-data")).unwrap(),
        contents
    );
    assert_eq!(
        fs::read(source.join(".application-data")).unwrap(),
        contents
    );
}

fn assert_ciphertext_progress(
    package: &Path,
    root: &Path,
    cancel: &AtomicBool,
    info: &PackageInfo,
) {
    let mut delivered = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(root.join("copy.sealed"))
        .unwrap();
    let mut copied = Vec::new();
    ImportReview::review(package, PASSWORD, Purpose::Home, limits(), cancel)
        .unwrap()
        .copy_ciphertext_reporting(&mut delivered, cancel, &mut |p| {
            copied.push(p);
            Ok(())
        })
        .unwrap();
    assert_eq!(copied.last().unwrap().completed, info.sealed_bytes);
    assert!(copied.iter().all(
        |p| p.phase == ArchivePhase::DeliveringCiphertext && p.total == Some(info.sealed_bytes)
    ));
    assert_eq!(
        fs::read(root.join("copy.sealed")).unwrap(),
        fs::read(package).unwrap()
    );
}

fn assert_archive_progress(progress: &[ArchiveProgress], info: &PackageInfo) {
    for phase in [ArchivePhase::Archiving, ArchivePhase::VerifyingArchive] {
        let phase_progress: Vec<_> = progress.iter().filter(|p| p.phase == phase).collect();
        assert_eq!(phase_progress.first().unwrap().completed, 0);
        assert_eq!(
            phase_progress.last().unwrap().completed,
            info.plaintext_bytes
        );
        assert!(
            phase_progress
                .windows(2)
                .all(|pair| pair[0].completed <= pair[1].completed)
        );
        assert!(phase_progress.iter().all(|p| p.total.is_none()));
    }
    assert_eq!(
        progress.last().unwrap().phase,
        ArchivePhase::VerifyingSource
    );
    let public = serde_json::to_string(progress).unwrap();
    assert!(!public.contains(".application-data"));
    assert!(!public.contains(std::str::from_utf8(PASSWORD).unwrap()));
}

fn assert_mid_import_cancelled(package: &Path, root: &Path, cancel: &Arc<AtomicBool>) {
    use std::{cell::Cell, rc::Rc, sync::atomic::Ordering};

    let ciphertext = fs::read(package).unwrap();
    let import_pid = Rc::new(Cell::new(None));
    let observed_pid = import_pid.clone();
    let observed_cancel = cancel.clone();
    let mut bytes_written = 0usize;
    let observer = process::StreamObserver::install(move |event| match event {
        process::StreamEvent::Spawned(child) => observed_pid.set(Some(child)),
        process::StreamEvent::Written(count) => {
            bytes_written = bytes_written.checked_add(count).unwrap();
            if bytes_written > 2 * codec::CHUNK {
                observed_cancel.store(true, Ordering::Release);
            }
        }
        process::StreamEvent::Read(_) => {}
    });
    let interrupted = root.join("interrupted-import");
    assert!(
        ImportReview::review(package, PASSWORD, Purpose::Home, limits(), cancel)
            .unwrap()
            .extract(Zeroizing::new(PASSWORD.to_vec()), &interrupted, cancel)
            .is_err()
    );
    drop(observer);
    assert!(cancel.load(Ordering::Acquire));
    let child = i32::try_from(import_pid.get().unwrap()).unwrap();
    // SAFETY: signal zero observes only the recorded, owned extraction child.
    assert_eq!(unsafe { libc::kill(child, 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    // Interrupted custody has no completed name. Its exact private attempt is
    // retained; retry uses the same requested quarantine, never an overwrite.
    assert!(!interrupted.exists());
    let attempts: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".tilo-import-")
        })
        .collect();
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        fs::metadata(&attempts[0]).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(fs::read(package).unwrap(), ciphertext);
    cancel.store(false, Ordering::Release);
    let mut retry =
        ImportReview::review(package, PASSWORD, Purpose::Home, limits(), cancel).unwrap();
    let completed = retry
        .extract_held(Zeroizing::new(PASSWORD.to_vec()), &interrupted, cancel)
        .unwrap();
    retry.verify_existing_tree(&completed.root, cancel).unwrap();
    let completed_inode = fs::metadata(&interrupted).unwrap().ino();
    assert!(
        retry
            .extract_held(Zeroizing::new(PASSWORD.to_vec()), &interrupted, cancel)
            .is_err()
    );
    assert_eq!(fs::metadata(&interrupted).unwrap().ino(), completed_inode);
    retry.verify_existing_tree(&completed.root, cancel).unwrap();
    assert!(attempts[0].exists());
    assert_eq!(fs::read(package).unwrap(), ciphertext);
}

#[test]
fn allocation_bound_and_existing_destination_are_enforced() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), vec![3; 4096]).unwrap();
    assert!(
        ExportReview::review(
            &source,
            &root.join("small"),
            Purpose::Home,
            Limits {
                plaintext_bytes: 10,
                ..limits()
            }
        )
        .is_err()
    );
    let package = root.join("sealed");
    let cancel = AtomicBool::new(false);
    ExportReview::review(&source, &package, Purpose::Home, limits())
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    let quarantine = root.join("existing");
    fs::create_dir(&quarantine).unwrap();
    fs::write(quarantine.join("untouched"), b"keep").unwrap();
    let review =
        ImportReview::review(&package, PASSWORD, Purpose::Home, limits(), &cancel).unwrap();
    assert!(
        review
            .extract(Zeroizing::new(PASSWORD.to_vec()), &quarantine, &cancel)
            .is_err()
    );
    assert_eq!(fs::read(quarantine.join("untouched")).unwrap(), b"keep");
}

#[test]
fn protected_private_custody_keeps_descriptor_bound_archive_export_and_import_working() {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().canonicalize().unwrap();
    let source = base.join("source");
    let target = base.join("target");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&target).unwrap();
    std::fs::create_dir(source.join(".config")).unwrap();
    std::fs::write(source.join(".config/document"), b"private fixture bytes").unwrap();
    std::fs::set_permissions(
        source.join(".config/document"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let source_pin = PinnedDirectory::open_absolute(&source).unwrap();
    let target_pin = PinnedDirectory::open_absolute(&target).unwrap();
    tilo_user_identity::persist::protect_private_custody().unwrap();
    // SAFETY: PR_GET_DUMPABLE reads this test process's current privacy mode.
    assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE) }, 0);
    let cancel = AtomicBool::new(false);
    let mut archive = Vec::new();
    process::create(
        &source_pin,
        &SourceInventory::capture_home(&source_pin).unwrap(),
        &mut archive,
        &cancel,
        true,
    )
    .unwrap();
    process::extract(&target_pin, &mut std::io::Cursor::new(archive), &cancel).unwrap();
    assert_eq!(
        std::fs::read(target.join(".config/document")).unwrap(),
        b"private fixture bytes"
    );
    assert_eq!(
        std::fs::metadata(target.join(".config/document"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::read(source.join(".config/document")).unwrap(),
        b"private fixture bytes"
    );
    source_pin.validate().unwrap();
    target_pin.validate().unwrap();
    // The bridge must not make the process dumpable to obtain tar access.
    assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE) }, 0);
}

#[test]
fn original_application_settings_proof_is_bound_to_authenticated_home_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.join("original-home");
    fs::create_dir_all(home.join(".local/share/flatpak/repo")).unwrap();
    let original = b"[core]\nmode=bare-user-only\n[remote \"personal\"]\ngpg-verify=true\n";
    fs::write(home.join(".local/share/flatpak/repo/config"), original).unwrap();
    let package = root.join("home.sealed");
    let cancel = AtomicBool::new(false);
    ExportReview::review(&home, &package, Purpose::Home, limits())
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    let review =
        ImportReview::review(&package, PASSWORD, Purpose::Home, limits(), &cancel).unwrap();
    review.verify_original_application_config(original).unwrap();
    assert!(
        review
            .verify_original_application_config(
                b"[core]\nmode=bare-user-only\n[remote \"personal\"]\ngpg-verify=false\n"
            )
            .is_err()
    );
    assert!(review.verify_original_application_config(b"").is_err());
    assert_eq!(
        fs::read(home.join(".local/share/flatpak/repo/config")).unwrap(),
        original
    );
}

#[test]
fn large_sparse_home_roundtrip_uses_stored_capacity_and_preserves_logical_data() {
    use std::{
        io::{Seek, SeekFrom},
        os::unix::fs::{FileExt, MetadataExt},
    };
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let source = root.path().join("source");
    fs::create_dir_all(source.join(".var/app/org.fixture")).unwrap();
    let image = source.join(".var/app/org.fixture/image");
    let logical = 1024u64 * 1024 * 1024;
    let mut original = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&image)
        .unwrap();
    original.write_all(b"sparse VM header sentinel").unwrap();
    original.set_len(logical).unwrap();
    original.seek(SeekFrom::Start(logical / 2)).unwrap();
    original.write_all(b"middle extent").unwrap();
    original.seek(SeekFrom::End(-4)).unwrap();
    original.write_all(b"tail").unwrap();
    original.sync_all().unwrap();
    fs::hard_link(&image, source.join("image-alias")).unwrap();
    let cancel = AtomicBool::new(false);
    let home = crate::setup::portable_home_media::HomeSource::review(&source).unwrap();
    assert_eq!(home.summary().unwrap().bytes, logical * 2);
    assert!(home.sealed_reservation().unwrap() < logical / 16);
    let package = root.path().join("home.sealed");
    let bounded = Limits {
        plaintext_bytes: 1024 * 1024,
        entries: 32,
        metadata_bytes: 512 * 1024,
    };
    let export = ExportReview::review(&source, &package, Purpose::Home, bounded).unwrap();
    let info = export
        .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    assert_eq!(info.logical_bytes, logical * 2);
    assert!(info.plaintext_bytes < 128 * 1024);
    assert!(info.sealed_bytes < 256 * 1024);
    assert_eq!(fs::metadata(&image).unwrap().len(), logical);
    let import = ImportReview::review(&package, PASSWORD, Purpose::Home, bounded, &cancel).unwrap();
    assert_eq!(import.info().logical_bytes, info.logical_bytes);
    let tree = import
        .extract(
            Zeroizing::new(PASSWORD.to_vec()),
            &root.path().join("quarantine"),
            &cancel,
        )
        .unwrap();
    let imported_path = tree.root.join(".var/app/org.fixture/image");
    let imported = File::open(&imported_path).unwrap();
    let metadata = imported.metadata().unwrap();
    assert_eq!(metadata.len(), logical);
    assert!(
        metadata.blocks() * 512 < 1024 * 1024,
        "Sparse import allocated its logical holes"
    );
    assert_eq!(
        metadata.ino(),
        fs::metadata(tree.root.join("image-alias")).unwrap().ino()
    );
    for offset in [0, 4096, logical / 2, logical - 32] {
        let mut expected = [0; 32];
        let mut actual = [0; 32];
        original.read_exact_at(&mut expected, offset).unwrap();
        imported.read_exact_at(&mut actual, offset).unwrap();
        assert_eq!(actual, expected);
    }
    assert!(fs::metadata(&image).unwrap().blocks() * 512 < 1024 * 1024);
}

#[test]
fn sparse_archive_requires_exact_authenticated_logical_source_bound() {
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let mut archive = tar::Builder::new(Vec::new());
    archive
        .append_data(&mut header(tar::EntryType::Directory, 0), ".", io::empty())
        .unwrap();
    archive
        .append_pax_extensions([
            ("GNU.sparse.size", b"8192".as_slice()),
            ("GNU.sparse.numblocks", b"2".as_slice()),
            ("GNU.sparse.map", b"0,4,8192,0".as_slice()),
            ("GNU.sparse.name", b"file".as_slice()),
        ])
        .unwrap();
    archive
        .append_data(
            &mut header(tar::EntryType::Regular, 4),
            "GNUSparseFile.fixture/file",
            b"data".as_slice(),
        )
        .unwrap();
    let archive = archive.into_inner().unwrap();
    let cancel = AtomicBool::new(false);
    for (index, logical) in [None, Some(4096), Some(8192)].into_iter().enumerate() {
        let mut seal = codec::Sealer::new(
            Vec::new(),
            PASSWORD,
            Purpose::Home,
            limits().plaintext_bytes,
            &cancel,
        )
        .unwrap();
        seal.write_all(&archive).unwrap();
        let (bytes, _) = seal.finish_bound(0, None, logical).unwrap();
        let package = root.path().join(format!("package-{index}"));
        fs::write(&package, bytes).unwrap();
        let result = ImportReview::review(&package, PASSWORD, Purpose::Home, limits(), &cancel);
        if logical == Some(8192) {
            assert_eq!(result.unwrap().info().logical_bytes, 8192);
        } else {
            assert!(
                result.is_err(),
                "Missing or mismatched logical bound must refuse before extraction"
            );
        }
    }
    assert_eq!(
        root.path().read_dir().unwrap().count(),
        3,
        "Review must not create any target tree"
    );
}

#[test]
fn non_home_setup_encoding_keeps_regular_legacy_footer_even_for_sparse_source() {
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let file = File::create(source.join("payload")).unwrap();
    file.set_len(65536).unwrap();
    let package = root.path().join("setup.sealed");
    let cancel = AtomicBool::new(false);
    let exported = ExportReview::review(&source, &package, Purpose::PreparedSetup, limits())
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    assert_eq!(exported.logical_bytes, 65536);
    assert!(exported.plaintext_bytes >= 65536);
    let mut file = File::open(package).unwrap();
    let (_, footer) = archive::scan(
        codec::Opener::new(&mut file, PASSWORD, limits().plaintext_bytes, &cancel).unwrap(),
        limits(),
    )
    .unwrap();
    assert!(footer.logical_bytes.is_none());
    assert!(
        serde_json::to_value(&footer)
            .unwrap()
            .get("logical_bytes")
            .is_none()
    );
}

#[test]
fn sorted_archive_index_preserves_earlier_hardlink_across_multiple_runs() {
    let private = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(private.path()).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let mut tar = tar::Builder::new(Vec::new());
    tar.append_data(&mut header(tar::EntryType::Directory, 0), ".", io::empty())
        .unwrap();
    tar.append_data(
        &mut header(tar::EntryType::Regular, 7),
        "z-original",
        b"private".as_slice(),
    )
    .unwrap();
    let mut link = header(tar::EntryType::Link, 0);
    link.set_link_name("z-original").unwrap();
    link.set_cksum();
    tar.append_data(&mut link, "a-alias", io::empty()).unwrap();
    for number in (0..700).rev() {
        tar.append_data(
            &mut header(tar::EntryType::Regular, 1),
            format!("file-{number:04}"),
            b"x".as_slice(),
        )
        .unwrap();
    }
    let package = root.join("home.sealed");
    fs::write(&package, package_of_archive(&tar.into_inner().unwrap())).unwrap();
    fs::set_permissions(&package, fs::Permissions::from_mode(0o600)).unwrap();
    let cancel = AtomicBool::new(false);
    let mut review =
        ImportReview::review(&package, PASSWORD, Purpose::Home, limits(), &cancel).unwrap();
    assert_eq!(review.info().entries, 703);
    let tree = review
        .extract_held(
            Zeroizing::new(PASSWORD.to_vec()),
            &root.join("quarantine"),
            &cancel,
        )
        .unwrap();
    assert_eq!(fs::read(tree.root.join("a-alias")).unwrap(), b"private");
    let original = fs::metadata(tree.root.join("z-original")).unwrap();
    let alias = fs::metadata(tree.root.join("a-alias")).unwrap();
    assert_eq!((original.dev(), original.ino()), (alias.dev(), alias.ino()));
    assert_eq!(original.nlink(), 2);
    for number in 0..700 {
        assert_eq!(
            fs::read(tree.root.join(format!("file-{number:04}"))).unwrap(),
            b"x"
        );
    }
    review.verify_existing_tree(&tree.root, &cancel).unwrap();
    fs::remove_file(tree.root.join("a-alias")).unwrap();
    fs::write(tree.root.join("a-alias"), b"private").unwrap();
    assert!(review.verify_existing_tree(&tree.root, &cancel).is_err());
}

#[test]
fn sorted_archive_index_refuses_later_parent_target_and_hardlink_chain_before_extraction() {
    for case in 0..3 {
        let private = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(private.path()).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let mut tar = tar::Builder::new(Vec::new());
        tar.append_data(&mut header(tar::EntryType::Directory, 0), ".", io::empty())
            .unwrap();
        if case == 0 {
            tar.append_data(
                &mut header(tar::EntryType::Regular, 1),
                "parent/file",
                b"x".as_slice(),
            )
            .unwrap();
            tar.append_data(
                &mut header(tar::EntryType::Directory, 0),
                "parent",
                io::empty(),
            )
            .unwrap();
        } else {
            if case == 2 {
                tar.append_data(
                    &mut header(tar::EntryType::Regular, 1),
                    "original",
                    b"x".as_slice(),
                )
                .unwrap();
            }
            let mut first = header(tar::EntryType::Link, 0);
            first.set_link_name("original").unwrap();
            first.set_cksum();
            tar.append_data(&mut first, "alias", io::empty()).unwrap();
            if case == 1 {
                tar.append_data(
                    &mut header(tar::EntryType::Regular, 1),
                    "original",
                    b"x".as_slice(),
                )
                .unwrap();
            } else {
                let mut second = header(tar::EntryType::Link, 0);
                second.set_link_name("alias").unwrap();
                second.set_cksum();
                tar.append_data(&mut second, "chain", io::empty()).unwrap();
            }
        }
        let package = root.join("home.sealed");
        let sealed = package_of_archive(&tar.into_inner().unwrap());
        fs::write(&package, &sealed).unwrap();
        fs::set_permissions(&package, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            ImportReview::review(
                &package,
                PASSWORD,
                Purpose::Home,
                limits(),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert_eq!(fs::read(&package).unwrap(), sealed);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }
}

#[cfg(all(target_os = "linux", feature = "native-capture-qualification"))]
#[test]
fn retained_archive_index_uses_exact_private_runs_and_refuses_insufficient_restore_space() {
    let native = PathBuf::from(
        std::env::var_os("TILO_FS_NATIVE_STORE")
            .expect("disposable supported native store required"),
    );
    let private = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(native)
        .unwrap();
    let root = fs::canonicalize(private.path()).unwrap();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(source.join("file"), b"actual archive data").unwrap();
    let package = root.join("home.sealed");
    let cancel = AtomicBool::new(false);
    ExportReview::review(&source, &package, Purpose::Home, limits())
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    let workspace = root.join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
    let pin = PinnedDirectory::open_absolute(&workspace).unwrap();
    let scratch = super::super::migration_review::InventoryScratch::review_for_restore(
        pin.clone(),
        limits().plaintext_bytes,
        limits().entries,
        limits().metadata_bytes,
        super::super::home_transfer::promotion::JOURNAL_RESERVE,
    )
    .unwrap();
    let mut review = ImportReview::review_in(
        &package,
        PASSWORD,
        Purpose::Home,
        limits(),
        &cancel,
        archive::Scratch::retained(scratch),
    )
    .unwrap();
    let tree = review
        .extract_held(
            Zeroizing::new(PASSWORD.to_vec()),
            &workspace.join("quarantine"),
            &cancel,
        )
        .unwrap();
    assert_eq!(
        fs::read(tree.root.join("file")).unwrap(),
        b"actual archive data"
    );
    review.verify_existing_tree(&tree.root, &cancel).unwrap();
    use std::os::fd::AsRawFd;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    assert_eq!(
        unsafe { libc::fstatvfs(pin.file.as_raw_fd(), stats.as_mut_ptr()) },
        0
    );
    let stats = unsafe { stats.assume_init() };
    let actual_free = stats.f_bavail.checked_mul(stats.f_frsize).unwrap();
    assert!(
        super::super::migration_review::InventoryScratch::review_for_restore(
            pin.clone(),
            1,
            1,
            1,
            actual_free,
        )
        .is_err()
    );
    let undersized = super::super::migration_review::InventoryScratch::review(pin, 1).unwrap();
    assert!(
        ImportReview::review_in(
            &package,
            PASSWORD,
            Purpose::Home,
            limits(),
            &cancel,
            archive::Scratch::retained(undersized)
        )
        .is_err()
    );
    assert_eq!(fs::read_dir(&workspace).unwrap().count(), 1);
    review.verify_existing_tree(&tree.root, &cancel).unwrap();
}

#[cfg(all(target_os = "linux", feature = "native-capture-qualification"))]
#[test]
fn authenticated_fragmented_sparse_archive_refuses_stored_bytes_only_restore_budget() {
    use std::os::fd::AsRawFd;
    let native = PathBuf::from(
        std::env::var_os("TILO_FS_NATIVE_STORE")
            .expect("disposable supported native store required"),
    );
    let owned = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(native)
        .unwrap();
    let root = fs::canonicalize(owned.path()).unwrap();
    let workspace = root.join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
    let pin = PinnedDirectory::open_absolute(&workspace).unwrap();
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    assert_eq!(
        unsafe { libc::fstatvfs(pin.file.as_raw_fd(), stats.as_mut_ptr()) },
        0
    );
    let block = unsafe { stats.assume_init() }.f_frsize;
    assert!(
        block >= 4096,
        "native qualification requires allocation blocks at least4KiB"
    );
    let count = 65_536u64;
    let logical = (count - 1).checked_mul(block * 2).unwrap() + 1;
    let map = (0..count)
        .map(|n| format!("{},1", n * block * 2))
        .collect::<Vec<_>>()
        .join(",");
    let logical_text = logical.to_string();
    let count_text = count.to_string();
    let mut tar = tar::Builder::new(Vec::new());
    tar.append_data(&mut header(tar::EntryType::Directory, 0), ".", io::empty())
        .unwrap();
    tar.append_pax_extensions([
        ("GNU.sparse.size", logical_text.as_bytes()),
        ("GNU.sparse.numblocks", count_text.as_bytes()),
        ("GNU.sparse.map", map.as_bytes()),
        ("GNU.sparse.name", b"fragmented".as_slice()),
    ])
    .unwrap();
    tar.append_data(
        &mut header(tar::EntryType::Regular, count),
        "GNUSparseFile.fixture/fragmented",
        vec![b'x'; count as usize].as_slice(),
    )
    .unwrap();
    let plain = tar.into_inner().unwrap();
    let cancel = AtomicBool::new(false);
    let mut seal = codec::Sealer::new(
        Vec::new(),
        PASSWORD,
        Purpose::Home,
        limits().plaintext_bytes,
        &cancel,
    )
    .unwrap();
    seal.write_all(&plain).unwrap();
    let sealed = seal.finish_bound(0, None, Some(logical)).unwrap().0;
    let package = root.join("home.sealed");
    fs::write(&package, &sealed).unwrap();
    fs::set_permissions(&package, fs::Permissions::from_mode(0o600)).unwrap();
    // The same real authenticated archive passes ordinary structural admission.
    let ordinary =
        ImportReview::review(&package, PASSWORD, Purpose::Home, limits(), &cancel).unwrap();
    assert_eq!(ordinary.info().logical_bytes, logical);
    drop(ordinary);
    assert_eq!(
        unsafe { libc::fstatvfs(pin.file.as_raw_fd(), stats.as_mut_ptr()) },
        0
    );
    let stats = unsafe { stats.assume_init() };
    let free = stats.f_bavail.checked_mul(block).unwrap();
    // More than enough for stored payload and actual index runs, less than the
    //256MiB covered by separated one-byte extents. No logical holes are reserved.
    let remaining = 64 * 1024 * 1024;
    let scratch = super::super::migration_review::InventoryScratch::review_for_restore(
        pin,
        plain.len() as u64,
        2,
        limits().metadata_bytes,
        free.checked_sub(remaining).unwrap(),
    )
    .unwrap();
    let error = ImportReview::review_in(
        &package,
        PASSWORD,
        Purpose::Home,
        limits(),
        &cancel,
        archive::Scratch::retained(scratch),
    )
    .err()
    .expect("fragmented extent block reserve must refuse");
    assert!(
        error.to_string().contains("capacity unavailable"),
        "{error}"
    );
    assert_eq!(fs::read_dir(&workspace).unwrap().count(), 0);
    assert_eq!(fs::read(&package).unwrap(), sealed);
}

#[cfg(all(target_os = "linux", feature = "native-capture-qualification"))]
fn commit_disposable_ballast(
    ballast: &File,
    free: &impl Fn() -> u64,
    journal: u64,
    plaintext: u64,
) {
    use std::{
        os::fd::AsRawFd,
        time::{Duration, Instant},
    };
    let lower = journal.checked_add(2 * 1024 * 1024).unwrap();
    let upper = journal.checked_add(plaintext).unwrap();
    let target = lower
        .checked_add(upper.checked_sub(lower).unwrap() / 2)
        .unwrap();
    let before = free();
    let maximum = before
        .checked_sub(lower)
        .expect("native fixture capacity required");
    let started = Instant::now();
    let mut committed = 0_u64;
    for step in 0..32 {
        ballast.sync_all().unwrap();
        assert_eq!(unsafe { libc::syncfs(ballast.as_raw_fd()) }, 0);
        let remaining = free();
        let allocated = ballast
            .metadata()
            .unwrap()
            .blocks()
            .checked_mul(512)
            .unwrap();
        assert!(
            allocated >= committed,
            "ballast must have actual allocated extents, not sparse length"
        );
        eprintln!(
            "disposable ballast step={step} before={before} remaining={remaining} journal={journal} plaintext={plaintext} committed={committed} allocated={allocated} lower={lower} upper={upper}"
        );
        if remaining > lower && remaining < upper {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "bounded ballast commitment deadline"
        );
        let next = if remaining > target {
            committed.checked_add(remaining - target).unwrap()
        } else {
            committed
                .checked_sub(target - remaining)
                .expect("cannot reclaim more than owned ballast")
        };
        assert!(
            next <= maximum,
            "ballast growth exceeds initial disposable allocation bound"
        );
        if next > committed {
            assert_eq!(
                unsafe {
                    libc::posix_fallocate(
                        ballast.as_raw_fd(),
                        i64::try_from(committed).unwrap(),
                        i64::try_from(next - committed).unwrap(),
                    )
                },
                0,
                "actual owned extent allocation required"
            );
        } else {
            ballast.set_len(next).unwrap();
        }
        committed = next;
    }
    panic!("bounded actual ballast adjustments did not reach original capacity bracket");
}

#[cfg(all(target_os = "linux", feature = "native-capture-qualification"))]
#[test]
fn completed_output_authentication_needs_metadata_space_not_another_payload_and_never_extracts() {
    use std::os::fd::AsRawFd;
    let native = PathBuf::from(
        std::env::var_os("TILO_FS_NATIVE_STORE").expect("owned supported native store required"),
    );
    let private = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(native)
        .unwrap();
    let root = fs::canonicalize(private.path()).unwrap();
    let source = root.join("source");
    let workspace = root.join("workspace");
    for path in [&source, &workspace] {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    const PAYLOAD: u64 = 32 * 1024 * 1024;
    let mut payload = File::create(source.join("file")).unwrap();
    io::copy(&mut io::repeat(71).take(PAYLOAD), &mut payload).unwrap();
    payload.sync_all().unwrap();
    drop(payload);
    let policy = Limits {
        plaintext_bytes: 64 * 1024 * 1024,
        entries: 16,
        metadata_bytes: 1024 * 1024,
    };
    let package = root.join("home.sealed");
    let cancel = AtomicBool::new(false);
    let exported = ExportReview::review(&source, &package, Purpose::Home, policy)
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    let pin = PinnedDirectory::open_absolute(&workspace).unwrap();
    let journal = super::super::home_transfer::promotion::JOURNAL_RESERVE;
    let scratch = super::super::migration_review::InventoryScratch::review_for_restore(
        pin.clone(),
        exported.plaintext_bytes,
        exported.entries,
        policy.metadata_bytes,
        journal,
    )
    .unwrap();
    let mut initial = ImportReview::review_in(
        &package,
        PASSWORD,
        Purpose::Home,
        policy,
        &cancel,
        archive::Scratch::retained(scratch),
    )
    .unwrap();
    let tree = initial
        .extract_held(
            Zeroizing::new(PASSWORD.to_vec()),
            &workspace.join("imported"),
            &cancel,
        )
        .unwrap();
    initial.verify_existing_tree(&tree.root, &cancel).unwrap();
    let candidate = tree.root.clone();
    drop(initial);
    drop(tree);
    // Fill only this disposable fixture's owned filesystem, retaining enough
    // real space for metadata+journals but less than a duplicate Home payload.
    let free = || {
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        assert_eq!(
            unsafe { libc::fstatvfs(pin.file.as_raw_fd(), stats.as_mut_ptr()) },
            0
        );
        let stats = unsafe { stats.assume_init() };
        stats.f_bavail.checked_mul(stats.f_frsize).unwrap()
    };
    let ballast = File::create(root.join("owned-ballast")).unwrap();
    commit_disposable_ballast(&ballast, &free, journal, exported.plaintext_bytes);
    let remaining = free();
    assert!(
        remaining > journal + 2 * 1024 * 1024 && remaining < journal + exported.plaintext_bytes,
        "actual disposable free-space subject must bracket metadata and duplicate payload budgets: remaining={remaining}, journal={journal}, plaintext={}",
        exported.plaintext_bytes
    );
    assert!(
        super::super::migration_review::InventoryScratch::review_for_restore(
            pin.clone(),
            exported.plaintext_bytes,
            exported.entries,
            policy.metadata_bytes,
            journal
        )
        .is_err()
    );
    let metadata = super::super::migration_review::InventoryScratch::review_for_restore(
        pin.clone(),
        0,
        0,
        policy.metadata_bytes,
        journal,
    )
    .unwrap();
    let held = PinnedDirectory::open_absolute(&candidate).unwrap();
    let mut review = ImportReview::review_in(
        &package,
        PASSWORD,
        Purpose::Home,
        policy,
        &cancel,
        archive::Scratch::verification_only(metadata.clone(), held).unwrap(),
    )
    .unwrap();
    review.verify_existing_tree(&candidate, &cancel).unwrap();
    assert!(
        review
            .extract_held(
                Zeroizing::new(PASSWORD.to_vec()),
                &workspace.join("second-copy"),
                &cancel
            )
            .is_err()
    );
    assert!(!workspace.join("second-copy").exists());
    // A held existing-root grant does not follow a same-name replacement.
    fs::rename(&candidate, workspace.join("retained-old-output")).unwrap();
    fs::create_dir(&candidate).unwrap();
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(review.verify_existing_tree(&candidate, &cancel).is_err());
    assert!(
        review
            .extract_held(
                Zeroizing::new(PASSWORD.to_vec()),
                &workspace.join("second-copy"),
                &cancel
            )
            .is_err()
    );
    assert!(!workspace.join("second-copy").exists());
    drop(review);
    drop(metadata);
    drop(ballast);
}

#[test]
fn received_descriptor_authenticates_held_archive_across_path_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("document"), b"authentic original content").unwrap();
    let package = root.join("home.sealed");
    let cancel = AtomicBool::new(false);
    ExportReview::review(&source, &package, Purpose::Home, limits())
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    let held = File::open(&package).unwrap();
    let original = root.join("held.sealed");
    fs::rename(&package, &original).unwrap();
    fs::write(&package, b"replacement path does not authorize bytes").unwrap();
    let mut review = ImportReview::review_file_in(
        held,
        PASSWORD,
        Purpose::Home,
        limits(),
        &cancel,
        archive::Scratch::default(),
    )
    .unwrap();
    let restored = review
        .extract_held(
            Zeroizing::new(PASSWORD.to_vec()),
            &root.join("quarantine"),
            &cancel,
        )
        .unwrap();
    review
        .verify_existing_tree(&restored.root, &cancel)
        .unwrap();
    assert_eq!(
        fs::read(restored.root.join("document")).unwrap(),
        b"authentic original content"
    );
    assert_eq!(
        fs::read(&package).unwrap(),
        b"replacement path does not authorize bytes"
    );
    // Mutating the exact held inode is caught even after an authenticated scan.
    fs::OpenOptions::new()
        .write(true)
        .open(&original)
        .unwrap()
        .write_all(b"changed")
        .unwrap();
    assert!(review.verify_held_ciphertext(&cancel).is_err());
}

#[test]
fn received_descriptor_refuses_directory_and_wrong_purpose() {
    let temporary = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    assert!(
        ImportReview::review_file_in(
            File::open(temporary.path()).unwrap(),
            PASSWORD,
            Purpose::Home,
            limits(),
            &cancel,
            archive::Scratch::default()
        )
        .is_err()
    );
    let source = temporary.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("document"), b"original").unwrap();
    let package = temporary.path().join("home.sealed");
    ExportReview::review(&source, &package, Purpose::Home, limits())
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    assert!(
        ImportReview::review_file_in(
            File::open(&package).unwrap(),
            PASSWORD,
            Purpose::PreparedSetup,
            limits(),
            &cancel,
            archive::Scratch::default()
        )
        .is_err()
    );
}
