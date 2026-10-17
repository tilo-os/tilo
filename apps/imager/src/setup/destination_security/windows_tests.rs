use super::super::migration::copy_folder;
use super::*;
use std::sync::atomic::AtomicBool;
use std::{fs, fs::File, path::PathBuf};

fn acl_fixture(rule: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let source = base.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("hello"), b"hello").unwrap();
    let parent = imager_backend::destination_security::create_acl_fixture(&base, rule).unwrap();
    (temp, source, parent)
}

/// Windows parent ACL checks and protected private ACL on creation.
#[test]
fn windows_parent_acls_and_private_creation_are_enforced() {
    // Phase `windows_refuses_parent_with_untrusted_delete_child_acl`.
    // Catches trusting ownership or READ_CONTROL alone while another account
    // has authority to replace children between validation and mkdir/open.
    {
        let (_temp, source, parent) = acl_fixture("(A;;0x40;;;WD)");
        fs::write(parent.join("sentinel"), b"keep").unwrap();
        assert!(
            copy_folder(
                &source,
                &parent.join("copy"),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
        );
        assert!(!parent.join("copy").exists());
        assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"keep");
    }

    // Phase `windows_allows_parent_add_subdirectory_without_replacement_rights`.
    // Ancestor child-creation rights alone cannot replace our protected child.
    {
        let (_temp, source, parent) = acl_fixture("(A;;0x4;;;WD)");
        let report = copy_folder(
            &source,
            &parent.join("copy"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(report.complete(), "{report:?}");
        assert_eq!(fs::read(parent.join("copy/hello")).unwrap(), b"hello");
    }

    // Phase `windows_creation_uses_protected_private_acl_despite_parent_inheritance`.
    // Catches inherited permissive ACLs or protecting the DACL only after
    // directory creation. The private descriptor overrides inheritance.
    {
        let (_temp, source, parent) = acl_fixture("(A;OICIIO;FA;;;WD)");
        let destination = parent.join("copy");
        let report = copy_folder(&source, &destination, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(report.complete(), "{report:?}");
        let dir =
            cap_primitives::fs::open_ambient_dir(&destination, cap_primitives::ambient_authority())
                .unwrap();
        check_directory(&dir, None, true).unwrap();
        let copied = File::open(destination.join("hello")).unwrap();
        check_private_file(&copied).unwrap();
        assert_eq!(fs::read(destination.join("hello")).unwrap(), b"hello");
    }
}
