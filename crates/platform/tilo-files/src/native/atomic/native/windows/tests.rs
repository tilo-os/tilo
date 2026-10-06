use super::*;
use std::io::Write;
use std::os::windows::ffi::OsStringExt;
use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};

fn fixture() -> (tempfile::TempDir, Directory) {
    let root = tempfile::tempdir().unwrap();
    let directory = Directory::open(root.path()).unwrap();
    (root, directory)
}

#[test]
fn private_create_validates_acl_before_bytes_and_is_exclusive() {
    let (_root, directory) = fixture();
    let mut file = directory.create_private(OsStr::new("secret")).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 0);
    acl::validate_private(&file, &acl::Principal::current().unwrap()).unwrap();
    let mut flags = 0;
    // SAFETY: retained handle and a valid output pointer.
    assert_ne!(
        unsafe { GetHandleInformation(file.as_raw_handle(), &raw mut flags) },
        0
    );
    assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
    file.write_all(b"secret").unwrap();
    assert_eq!(
        directory
            .create_private(OsStr::new("secret"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    drop(file);
    let read = directory.open_private(OsStr::new("secret")).unwrap();
    assert_eq!(read.metadata().unwrap().len(), 6);
}

#[test]
fn private_read_rejects_additional_hardlinks_and_nonregular_objects() {
    let (root, directory) = fixture();
    drop(directory.create_private(OsStr::new("secret")).unwrap());
    std::fs::hard_link(root.path().join("secret"), root.path().join("alias")).unwrap();
    assert!(directory.open_private(OsStr::new("secret")).is_err());
    std::fs::create_dir(root.path().join("subdir")).unwrap();
    assert!(directory.open_private(OsStr::new("subdir")).is_err());
}

#[test]
fn relative_names_preserve_unpaired_utf16_and_reject_namespace_escape() {
    let (_root, directory) = fixture();
    let name = std::ffi::OsString::from_wide(&[0x78, 0xd800]);
    let file = directory.create_private(&name).unwrap();
    let id = Identity::of(&file).unwrap();
    drop(file);
    assert_eq!(
        id,
        Identity::of(&directory.open_private(&name).unwrap()).unwrap()
    );
    for invalid in ["", ".", "..", "a/b", "a\\b", "a:b", "a\0b"] {
        assert_eq!(
            directory
                .create_private(OsStr::new(invalid))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn private_read_rejects_permissive_and_null_dacls() {
    let (root, directory) = fixture();
    for (name, sddl) in [
        ("everyone", "D:P(A;;FA;;;WD)"),
        ("null", "D:NO_ACCESS_CONTROL"),
    ] {
        let file = std::fs::File::create(root.path().join(name)).unwrap();
        acl::tests::set_dacl(&root.path().join(name), sddl);
        drop(file);
        assert!(directory.open_private(OsStr::new(name)).is_err());
    }
}

#[test]
fn private_creation_blocks_inherited_other_principal_access() {
    let (root, directory) = fixture();
    acl::tests::set_dacl(root.path(), "D:P(A;OICI;FA;;;WD)");
    let file = directory.create_private(OsStr::new("secret")).unwrap();
    acl::validate_private(&file, &acl::Principal::current().unwrap()).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 0);
}

/// Native qualification requires a provisioned runner that executes its arguments
/// as a *different* unprivileged account, preserving the three fixture variables.
/// An absent runner fails qualification; this is intentionally not ignored.
#[test]
fn private_file_denies_a_distinct_native_principal() {
    const CHILD: &str = "TILO_ATOMIC_PRIVATE_CHILD_PATH";
    const OWNER: &str = "TILO_ATOMIC_PRIVATE_OWNER_SID";
    if let Some(path) = std::env::var_os(CHILD) {
        let principal = acl::Principal::current().unwrap();
        assert_ne!(principal.sid_text(), std::env::var(OWNER).unwrap());
        let path = std::path::PathBuf::from(path);
        // Prove the parent is accessible, so denial is not merely ancestry.
        Directory::open(path.parent().unwrap()).unwrap();
        assert_eq!(
            File::open(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        std::fs::write(
            path.with_file_name("principal-witness"),
            principal.sid_text(),
        )
        .unwrap();
        return;
    }
    let runner = std::env::var_os("TILO_ATOMIC_PRIVATE_SECOND_USER_RUNNER")
        .expect("qualification requires a real distinct-principal runner");
    let (root, directory) = fixture();
    acl::tests::set_dacl(root.path(), "D:P(A;OICI;FA;;;WD)");
    let mut file = directory.create_private(OsStr::new("secret")).unwrap();
    file.write_all(b"secret").unwrap();
    drop(file);
    let status = std::process::Command::new(runner)
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native::windows::tests::private_file_denies_a_distinct_native_principal",
            "--nocapture",
        ])
        .env(CHILD, root.path().join("secret"))
        .env(OWNER, acl::Principal::current().unwrap().sid_text())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "distinct-principal access qualification failed: {status}"
    );
    let observed = std::fs::read_to_string(root.path().join("principal-witness"))
        .expect("runner must execute and observe the distinct-principal access check");
    assert!(observed.starts_with("S-1-"));
    assert_ne!(observed, acl::Principal::current().unwrap().sid_text());
}

#[test]
fn private_creation_rejects_impersonation_before_mutation() {
    use windows_sys::Win32::Security::{ImpersonateSelf, RevertToSelf, SecurityImpersonation};
    let (root, directory) = fixture();
    struct Revert;
    impl Drop for Revert {
        fn drop(&mut self) {
            // SAFETY: test owns this thread's self-impersonation lifetime.
            assert_ne!(unsafe { RevertToSelf() }, 0);
        }
    }
    // SAFETY: impersonates our own token on this test thread only.
    assert_ne!(unsafe { ImpersonateSelf(SecurityImpersonation) }, 0);
    let _revert = Revert;
    assert_eq!(
        directory
            .create_private(OsStr::new("secret"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::Unsupported
    );
    assert!(!root.path().join("secret").exists());
}

#[test]
fn held_parent_creation_survives_ambient_path_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("parent");
    std::fs::create_dir(&path).unwrap();
    let directory = Directory::open(&path).unwrap();
    std::fs::rename(&path, root.path().join("moved")).unwrap();
    std::fs::create_dir(&path).unwrap();
    drop(directory.create_private(OsStr::new("secret")).unwrap());
    assert!(root.path().join("moved/secret").exists());
    assert!(!path.join("secret").exists());
}

#[test]
fn private_read_rejects_reparse_objects() {
    let (root, directory) = fixture();
    drop(directory.create_private(OsStr::new("secret")).unwrap());
    // This qualification requires symlink privilege/developer mode; absence
    // fails the prerequisite rather than silently skipping reparse coverage.
    std::os::windows::fs::symlink_file(root.path().join("secret"), root.path().join("link"))
        .unwrap();
    assert!(directory.open_private(OsStr::new("link")).is_err());
}

#[test]
fn held_directory_privacy_validates_security_without_file_link_policy() {
    let principal = acl::Principal::current().unwrap();
    for (dacl, accepted) in [
        (format!("D:P(A;;FA;;;{})", principal.sid_text()), true),
        (
            format!("D:P(D;;FW;;;WD)(A;;FA;;;{})", principal.sid_text()),
            true,
        ),
        ("D:P(A;;FA;;;WD)".to_owned(), false),
        ("D:NO_ACCESS_CONTROL".to_owned(), false),
    ] {
        let (root, directory) = fixture();
        std::fs::create_dir(root.path().join("child")).unwrap();
        acl::tests::set_dacl(root.path(), &dacl);
        let result = directory.validate_privacy(Privacy::OwnerOnly);
        assert_eq!(result.is_ok(), accepted, "{dacl}: {result:?}");
        directory.validate_privacy(Privacy::Inherit).unwrap();
    }
}

#[test]
fn held_directory_privacy_requires_security_query_access() {
    let (root, _directory) = fixture();
    let handle = File::options()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(root.path())
        .unwrap();
    let directory = Directory::from_file(handle).unwrap();
    assert!(directory.validate_privacy(Privacy::OwnerOnly).is_err());
}

#[test]
fn held_directory_privacy_rejects_unprotected_security() {
    use windows_sys::Win32::Security::{
        Authorization::{SE_FILE_OBJECT, SetSecurityInfo},
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
        UNPROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;
    let (root, directory) = fixture();
    let principal = acl::Principal::current().unwrap();
    acl::tests::set_dacl(
        root.path(),
        &format!("D:P(A;;FA;;;{})", principal.sid_text()),
    );
    let writable = File::options()
        .access_mode(WRITE_DAC | READ_CONTROL)
        .share_mode(7)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(root.path())
        .unwrap();
    let descriptor = principal.private_descriptor().unwrap();
    let mut dacl = std::ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    // SAFETY: live descriptor and correctly typed writable native outputs.
    assert_ne!(
        unsafe {
            GetSecurityDescriptorDacl(
                descriptor.as_ptr(),
                &raw mut present,
                &raw mut dacl,
                &raw mut defaulted,
            )
        },
        0
    );
    assert_ne!(present, 0);
    // SAFETY: fixture handle with WRITE_DAC and live descriptor-owned DACL.
    assert_eq!(
        unsafe {
            SetSecurityInfo(
                writable.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null_mut(),
            )
        },
        0
    );
    assert!(directory.validate_privacy(Privacy::OwnerOnly).is_err());
}

#[test]
fn held_directory_admission_rejects_file_and_reparse_directory() {
    let (root, directory) = fixture();
    let file = directory.create_private(OsStr::new("file")).unwrap();
    assert!(Directory::from_file(file).is_err());
    std::fs::create_dir(root.path().join("child")).unwrap();
    std::os::windows::fs::symlink_dir(root.path().join("child"), root.path().join("alias"))
        .unwrap();
    assert!(Directory::open(&root.path().join("alias")).is_err());
}
