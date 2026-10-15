//! Real Ext4/XFS live fallback. No snapshot claim, no prerequisite skips.
use crate::setup::{
    portable_home_media::{HomeSource, MediaGrant},
    portable_transfer::{ImportReview, Purpose},
};
use std::{
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileExt, MetadataExt, PermissionsExt, symlink},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
};
use zeroize::Zeroizing;

const CHILD: &str = "TILO_LIVE_HOME_QUALIFICATION_CHILD";
const PASSWORD: &[u8] = b"actual isolated native migration password";
const BINDING: &str = "native-live-home-qualification";
const LOGICAL: u64 = 1024 * 1024 * 1024;

fn acl(path: &Path, name: &'static std::ffi::CStr) -> Vec<u8> {
    let file = File::open(path).unwrap();
    let mut bytes = vec![0u8; 64 * 1024];
    // SAFETY: fixed ACL name, held file, bounded writable buffer.
    let count = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    assert!(
        count >= 0,
        "native ACL unavailable: {}",
        std::io::Error::last_os_error()
    );
    bytes.truncate(usize::try_from(count).unwrap());
    bytes
}

fn set_acl(path: &Path, value: &str) {
    let result = Command::new("setfacl")
        .arg("-m")
        .arg(value)
        .arg("--")
        .arg(path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn dataset(home: &Path) -> PathBuf {
    let data = home.join(".var/app/so.test/data");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("session"), b"hidden complete application bytes").unwrap();
    fs::write(home.join("script"), b"original executable user document").unwrap();
    fs::set_permissions(home.join("script"), fs::Permissions::from_mode(0o750)).unwrap();
    let file = File::open(home.join("script")).unwrap();
    let attribute = b"original user metadata";
    // SAFETY: held fixture file, fixed user attribute and valid byte slice.
    assert_eq!(
        unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                c"user.tilo.native".as_ptr(),
                attribute.as_ptr().cast(),
                attribute.len(),
                0,
            )
        },
        0
    );
    fs::hard_link(home.join("script"), home.join("same-document")).unwrap();
    symlink(".var/app/so.test/data/session", home.join("session-link")).unwrap();
    let image = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(data.join("sparse-image"))
        .unwrap();
    image.set_len(LOGICAL).unwrap();
    image.write_all_at(b"sparse head", 0).unwrap();
    image
        .write_all_at(b"sparse tail", LOGICAL.checked_sub(4096).unwrap())
        .unwrap();
    image.sync_all().unwrap();
    assert!(image.metadata().unwrap().blocks().checked_mul(512).unwrap() < LOGICAL / 16);
    // Supported principal-neutral extended ACLs. Named principal mapping remains
    // a separate unsupported contract; this fixture does not fabricate a map.
    set_acl(&data, "d:u::rwx,d:g::r-x,d:o::---");
    set_acl(&home.join("script"), "m::r--");
    data
}

fn verify(source: &Path, imported: &Path, data: &Path) {
    assert_eq!(
        fs::read(imported.join(".var/app/so.test/data/session")).unwrap(),
        b"hidden complete application bytes"
    );
    assert_eq!(
        fs::read(imported.join("script")).unwrap(),
        b"original executable user document"
    );
    assert_eq!(
        fs::read_link(imported.join("session-link")).unwrap(),
        Path::new(".var/app/so.test/data/session")
    );
    let original = fs::metadata(source.join("script")).unwrap();
    let copied = fs::metadata(imported.join("script")).unwrap();
    assert_eq!(original.mode() & 0o777, copied.mode() & 0o777);
    assert_eq!(
        acl(&source.join("script"), c"user.tilo.native"),
        acl(&imported.join("script"), c"user.tilo.native")
    );
    assert_eq!(
        copied.ino(),
        fs::metadata(imported.join("same-document")).unwrap().ino()
    );
    assert_eq!(
        acl(&source.join("script"), c"system.posix_acl_access"),
        acl(&imported.join("script"), c"system.posix_acl_access")
    );
    assert_eq!(
        acl(data, c"system.posix_acl_default"),
        acl(
            &imported.join(".var/app/so.test/data"),
            c"system.posix_acl_default"
        )
    );
    let image = File::open(imported.join(".var/app/so.test/data/sparse-image")).unwrap();
    let metadata = image.metadata().unwrap();
    assert_eq!(metadata.len(), LOGICAL);
    assert!(metadata.blocks().checked_mul(512).unwrap() < LOGICAL / 16);
    let mut head = [0u8; 11];
    image.read_exact_at(&mut head, 0).unwrap();
    assert_eq!(&head, b"sparse head");
    let mut hole = [1u8; 4096];
    image.read_exact_at(&mut hole, LOGICAL / 2).unwrap();
    assert_eq!(hole, [0u8; 4096]);
    let mut tail = [0u8; 11];
    image
        .read_exact_at(&mut tail, LOGICAL.checked_sub(4096).unwrap())
        .unwrap();
    assert_eq!(&tail, b"sparse tail");
    assert_eq!(
        File::open(data.join("sparse-image"))
            .unwrap()
            .metadata()
            .unwrap()
            .len(),
        LOGICAL
    );
}

fn live_story(root: &Path) {
    assert_eq!(rustix::process::getuid().as_raw(), 1000);
    assert_eq!(rustix::process::geteuid().as_raw(), 1000);
    let home = root.join("home");
    let media = root.join("media");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&media).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&media, fs::Permissions::from_mode(0o700)).unwrap();
    let data = dataset(&home);
    let cancel = AtomicBool::new(false);
    // The actual media producer on genuine Ext4/XFS must retain permission
    // without reading mutable/private file contents or inventing archive facts.
    let opaque = home.join("private-changing-database");
    fs::write(&opaque, b"mutable source bytes").unwrap();
    fs::set_permissions(&opaque, fs::Permissions::from_mode(0o000)).unwrap();
    let original = HomeSource::review_for_media_preparation(&home, &cancel).unwrap();
    assert!(original.original_only());
    assert!(!original.immutable_capture());
    assert_eq!(original.capture_status(), "awaiting-original-boot");
    assert!(original.summary().is_err());
    assert!(original.sealed_reservation().is_err());
    fs::write(home.join("script"), b"later user edit before boot").unwrap();
    original.validate_with_cancel(&cancel).unwrap();
    let empty_media = MediaGrant::open(&media).unwrap();
    assert!(
        original
            .export_to(
                &empty_media,
                "unadmitted-original",
                BINDING,
                Zeroizing::new(PASSWORD.to_vec()),
                &cancel
            )
            .is_err()
    );
    assert!(fs::read_dir(&media).unwrap().next().is_none());
    assert!(HomeSource::review_for_media_preparation(&home, &AtomicBool::new(true)).is_err());
    // Retiring the granted inode must still refuse; matching path spelling
    // cannot substitute a new directory for the original user's permission.
    let retired = root.join("retired-original-home");
    fs::rename(&home, &retired).unwrap();
    fs::create_dir(&home).unwrap();
    assert!(original.validate().is_err());
    fs::remove_dir(&home).unwrap();
    fs::rename(&retired, &home).unwrap();
    fs::set_permissions(&opaque, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&opaque).unwrap();
    fs::write(home.join("script"), b"original executable user document").unwrap();
    // Existing explicit live-export behavior is separately qualified below.
    let reviewed = HomeSource::review_for_preparation(&home, &cancel).unwrap();
    assert_eq!(reviewed.capture_status(), "live");
    assert!(!reviewed.immutable_capture());
    let grant = MediaGrant::open(&media).unwrap();
    let expected = reviewed
        .export_to(
            &grant,
            "native-live",
            BINDING,
            Zeroizing::new(PASSWORD.to_vec()),
            &cancel,
        )
        .unwrap();
    assert!(
        expected.sealed_bytes < 4 * 1024 * 1024,
        "sparse data must not expand to logical bytes"
    );
    let imported = ImportReview::review(
        &media.join(&expected.filename),
        PASSWORD,
        Purpose::Home,
        expected.limits,
        &cancel,
    )
    .unwrap();
    assert_eq!(imported.ciphertext_digest(), expected.ciphertext_sha256);
    assert_eq!(imported.info().setup_binding.as_deref(), Some(BINDING));
    let tree = imported
        .extract(
            Zeroizing::new(PASSWORD.to_vec()),
            &root.join("imported"),
            &cancel,
        )
        .unwrap();
    verify(&home, &tree.root, &data);
    // Actual source mutation must invalidate the reviewed live grant, never
    // publish a completion receipt or an apparently successful new generation.
    fs::write(
        home.join("script"),
        b"edited in the original operating system",
    )
    .unwrap();
    let error = reviewed
        .export_to(
            &grant,
            "changed-live",
            BINDING,
            Zeroizing::new(PASSWORD.to_vec()),
            &cancel,
        )
        .unwrap_err();
    assert!(crate::setup::portable_home_media::source_changed(&error));
    assert!(!media.join("home-changed-live.sealed").exists());
    assert_eq!(fs::read_dir(&media).unwrap().count(), 1);
    assert_eq!(
        fs::read(tree.root.join("script")).unwrap(),
        b"original executable user document"
    );
}

fn qualify(filesystem: &str, selector: &str) {
    if let Some(root) = std::env::var_os(CHILD) {
        live_story(Path::new(&root));
        println!("LIVE_HOME_QUALIFIED:{filesystem}");
        return;
    }
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "explicit privileged native qualifier required"
    );
    let key = format!("TILO_FS_NATIVE_{}_STORE", filesystem.to_ascii_uppercase());
    let store = PathBuf::from(
        std::env::var_os(&key).expect("canonical disposable filesystem fixture is required"),
    );
    let held = File::open(&store).unwrap();
    let mut facts = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: held directory descriptor and correctly-sized kernel result.
    assert_eq!(
        unsafe { libc::fstatfs(held.as_raw_fd(), facts.as_mut_ptr()) },
        0
    );
    let facts = unsafe { facts.assume_init() };
    assert_eq!(
        facts.f_type,
        if filesystem == "ext4" {
            0xef53
        } else {
            0x58465342
        }
    );
    let temporary = tempfile::Builder::new()
        .prefix("live-home-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(&store)
        .unwrap();
    let executable = temporary.path().join("qualifier");
    fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::ffi::CString::new(temporary.path().as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path.as_ptr(), 1000, 1000) }, 0);
    let result = Command::new(executable)
        .args(["--exact", selector, "--nocapture"])
        .env(CHILD, temporary.path())
        .env("HOME", temporary.path().join("home"))
        .env("XDG_STATE_HOME", temporary.path().join("state"))
        .uid(1000)
        .gid(1000)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout:{}\nstderr:{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout)
            .contains(&format!("LIVE_HOME_QUALIFIED:{filesystem}")),
        "selected child test did not observe the real filesystem"
    );
}

#[test]
fn real_ext4_live_home_export_import_and_mutation_refusal() {
    qualify(
        "ext4",
        "setup::home_capture::native_tests::live_filesystems::real_ext4_live_home_export_import_and_mutation_refusal",
    );
}

#[test]
fn real_xfs_live_home_export_import_and_mutation_refusal() {
    qualify(
        "xfs",
        "setup::home_capture::native_tests::live_filesystems::real_xfs_live_home_export_import_and_mutation_refusal",
    );
}
