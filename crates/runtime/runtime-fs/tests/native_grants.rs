#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "owned integration fixtures and assertion helpers"
)]

mod support;

use runtime_fs::{
    DirectoryGrant, EntryKind, EntryName, FileGrant, FsError, FsRight, GrantedEntry,
    MAX_READ_CHUNK, NativeGrantAuthority,
};
use std::collections::HashSet;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::symlink;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use support::{MOUNT_A, MOUNT_B, admit, bind, files_owner, granted_root};

fn name(value: &str) -> EntryName {
    EntryName::new(value.into()).unwrap()
}
fn file(entry: GrantedEntry) -> FileGrant {
    match entry {
        GrantedEntry::File(file) => file,
        GrantedEntry::Directory(_) => panic!("expected file"),
    }
}
fn directory(entry: GrantedEntry) -> DirectoryGrant {
    match entry {
        GrantedEntry::Directory(dir) => dir,
        GrantedEntry::File(_) => panic!("expected directory"),
    }
}
fn fill_pages(path: &std::path::Path) {
    for i in 0..257 {
        std::fs::write(path.join(format!("page-{i:03}")), b"").unwrap();
    }
}

#[test]
fn revocation_invalidates_every_derived_handle_and_binding() {
    // phase: revoked_root_rejects_existing_descendant_handle
    {
        let (temp, authority, session, root) = granted_root();
        std::fs::write(temp.path().join("draft"), b"before").unwrap();
        let file = file(session.lookup(&root, &name("draft")).unwrap());
        assert_eq!(session.read_at(&file, 0, 16).unwrap(), b"before");
        let cloned = file.clone();
        authority.revoke_directory(&root).unwrap();
        assert!(matches!(
            session.read_at(&file, 0, u32::MAX),
            Err(FsError::Revoked)
        ));
        assert!(matches!(
            session.read_at(&cloned, 0, 16),
            Err(FsError::Revoked)
        ));
        assert!(matches!(
            session.metadata(&GrantedEntry::File(file)),
            Err(FsError::Revoked)
        ));
        assert!(matches!(
            session.lookup(&root, &name("draft")),
            Err(FsError::Revoked)
        ));
        assert!(matches!(session.list(&root, None), Err(FsError::Revoked)));
        assert!(matches!(
            session.check_directory(&root),
            Err(FsError::Revoked)
        ));
        assert_eq!(std::fs::read(temp.path().join("draft")).unwrap(), b"before");
    }
    // phase: revoked_subdirectory_invalidates_descendants_but_not_siblings
    {
        let (temp, authority, session, root) = granted_root();
        std::fs::create_dir(temp.path().join("child")).unwrap();
        std::fs::write(temp.path().join("child/file"), b"child").unwrap();
        std::fs::write(temp.path().join("sibling"), b"sibling").unwrap();
        let child = directory(session.lookup(&root, &name("child")).unwrap());
        let descendant = file(session.lookup(&child, &name("file")).unwrap());
        let scoped = bind(&authority, &child, MOUNT_B);
        authority.revoke_directory(&child).unwrap();
        assert!(matches!(
            session.read_at(&descendant, 0, 20),
            Err(FsError::Revoked)
        ));
        assert!(matches!(scoped.list(&child, None), Err(FsError::Revoked)));
        session.check_directory(&root).unwrap();
        assert_eq!(
            session
                .read_at(
                    &file(session.lookup(&root, &name("sibling")).unwrap()),
                    0,
                    20
                )
                .unwrap(),
            b"sibling"
        );
    }
    // phase: foreign_authority_and_revoked_grants_cannot_bind
    {
        let (temp, authority, _session, root) = granted_root();
        let foreign = NativeGrantAuthority::new();
        assert!(matches!(
            foreign.bind(files_owner(), MOUNT_B, std::slice::from_ref(&root), &[]),
            Err(FsError::Denied)
        ));
        authority.revoke_directory(&root).unwrap();
        assert!(matches!(
            authority.bind(files_owner(), MOUNT_B, &[root], &[]),
            Err(FsError::Revoked)
        ));
        std::fs::write(temp.path().join("direct"), b"direct").unwrap();
        let direct = authority
            .admit_file(
                std::fs::File::open(temp.path().join("direct")).unwrap(),
                files_owner(),
                vec![FsRight::Read],
            )
            .unwrap();
        let reader = authority
            .bind(files_owner(), MOUNT_A, &[], std::slice::from_ref(&direct))
            .unwrap();
        authority.revoke_file(&direct).unwrap();
        assert!(matches!(
            reader.read_at(&direct, 0, 10),
            Err(FsError::Revoked)
        ));
        assert!(matches!(
            authority.bind(files_owner(), MOUNT_B, &[], &[direct]),
            Err(FsError::Revoked)
        ));
    }
}

#[test]
fn grants_confine_each_owner_session_and_right() {
    // phase: wrong_owner_cannot_list
    {
        let (_temp, authority, _session, root) = granted_root();
        let mut wrong_workspace = files_owner();
        wrong_workspace.workspace = "other".into();
        let mut wrong_app = files_owner();
        wrong_app.app_id = "other".into();
        for owner in [wrong_workspace, wrong_app] {
            assert!(matches!(
                authority.bind(owner, MOUNT_B, std::slice::from_ref(&root), &[]),
                Err(FsError::Denied)
            ));
        }
    }
    // phase: same_owner_disjoint_sessions_cannot_use_each_others_roots
    {
        let temp = tempfile::tempdir().unwrap();
        for child in ["left", "right"] {
            std::fs::create_dir(temp.path().join(child)).unwrap();
            std::fs::write(temp.path().join(child).join(format!("{child}.txt")), child).unwrap();
        }
        let authority = NativeGrantAuthority::new();
        let left = admit(&authority, &temp.path().join("left"));
        let right = admit(&authority, &temp.path().join("right"));
        let a = bind(&authority, &left, MOUNT_A);
        let b = bind(&authority, &right, MOUNT_B);
        assert!(matches!(a.list(&right, None), Err(FsError::Denied)));
        assert!(matches!(b.list(&left, None), Err(FsError::Denied)));
        assert_eq!(a.list(&left, None).unwrap().entries.len(), 1);
        assert_eq!(b.list(&right, None).unwrap().entries.len(), 1);
        let left_file = file(a.lookup(&left, &name("left.txt")).unwrap());
        assert!(matches!(b.read_at(&left_file, 0, 4), Err(FsError::Denied)));
    }
    // phase: isolated_file_cannot_enumerate_parent
    {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("only.txt"), b"only").unwrap();
        let authority = NativeGrantAuthority::new();
        let parent = admit(&authority, temp.path());
        let grant = authority
            .admit_file(
                std::fs::File::open(temp.path().join("only.txt")).unwrap(),
                files_owner(),
                vec![FsRight::Read],
            )
            .unwrap();
        let session = authority
            .bind(files_owner(), MOUNT_A, &[], std::slice::from_ref(&grant))
            .unwrap();
        assert_eq!(session.read_at(&grant, 0, 10).unwrap(), b"only");
        assert_eq!(
            session.metadata(&GrantedEntry::File(grant)).unwrap().kind,
            EntryKind::File
        );
        assert!(matches!(session.list(&parent, None), Err(FsError::Denied)));
    }
    // phase: enumerate_only_cannot_read
    {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("alpha.txt"), b"alpha").unwrap();
        let authority = NativeGrantAuthority::new();
        let root = authority
            .admit_directory(
                std::fs::File::open(temp.path()).unwrap(),
                files_owner(),
                vec![FsRight::Enumerate],
            )
            .unwrap();
        let session = bind(&authority, &root, MOUNT_A);
        assert_eq!(session.list(&root, None).unwrap().entries.len(), 1);
        let grant = file(session.lookup(&root, &name("alpha.txt")).unwrap());
        assert!(matches!(
            session.read_at(&grant, 0, u32::MAX),
            Err(FsError::Denied)
        ));
        let direct = authority
            .admit_file(
                std::fs::File::open(temp.path().join("alpha.txt")).unwrap(),
                files_owner(),
                vec![FsRight::Read],
            )
            .unwrap();
        let reader = authority
            .bind(files_owner(), MOUNT_B, &[], std::slice::from_ref(&direct))
            .unwrap();
        assert_eq!(reader.read_at(&direct, 0, 10).unwrap(), b"alpha");
    }
}

fn symlink_lab() -> (
    tempfile::TempDir,
    NativeGrantAuthority,
    runtime_fs::FsSession,
    DirectoryGrant,
) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("root/nested")).unwrap();
    std::fs::create_dir(temp.path().join("outside")).unwrap();
    std::fs::write(temp.path().join("root/nested/inside.txt"), b"inside").unwrap();
    std::fs::write(temp.path().join("outside/secret.txt"), b"secret").unwrap();
    symlink("nested/inside.txt", temp.path().join("root/inside-link")).unwrap();
    symlink(
        "../outside/secret.txt",
        temp.path().join("root/escape-link"),
    )
    .unwrap();
    symlink(
        temp.path().join("outside/secret.txt"),
        temp.path().join("root/absolute-link"),
    )
    .unwrap();
    symlink("missing.txt", temp.path().join("root/dangling-link")).unwrap();
    let authority = NativeGrantAuthority::new();
    let root = admit(&authority, &temp.path().join("root"));
    let session = bind(&authority, &root, MOUNT_A);
    (temp, authority, session, root)
}

#[test]
fn symlinks_resolve_only_inside_the_session_root() {
    // phase: symlink_escape_is_denied
    {
        let (_temp, _authority, session, root) = symlink_lab();
        let listing = session.list(&root, None).unwrap();
        for label in ["escape-link", "absolute-link"] {
            assert!(
                listing
                    .entries
                    .iter()
                    .any(|row| row.name == name(label) && row.metadata.kind == EntryKind::Symlink)
            );
            let grant = file(session.lookup(&root, &name(label)).unwrap());
            assert_eq!(
                session
                    .metadata(&GrantedEntry::File(grant.clone()))
                    .unwrap()
                    .kind,
                EntryKind::Symlink
            );
            assert!(matches!(
                session.read_at(&grant, 0, 100),
                Err(FsError::Denied)
            ));
        }
    }
    // phase: in_root_relative_symlink_reads_target
    {
        let (_temp, _authority, session, root) = symlink_lab();
        let grant = file(session.lookup(&root, &name("inside-link")).unwrap());
        assert_eq!(
            session
                .metadata(&GrantedEntry::File(grant.clone()))
                .unwrap()
                .kind,
            EntryKind::Symlink
        );
        assert_eq!(session.read_at(&grant, 0, 100).unwrap(), b"inside");
    }
    // phase: dangling_symlink_is_listable_but_not_readable
    {
        let (_temp, _authority, session, root) = symlink_lab();
        assert!(
            session
                .list(&root, None)
                .unwrap()
                .entries
                .iter()
                .any(|row| row.name == name("dangling-link")
                    && row.metadata.kind == EntryKind::Symlink)
        );
        let grant = file(session.lookup(&root, &name("dangling-link")).unwrap());
        assert!(matches!(
            session.read_at(&grant, 0, 10),
            Err(FsError::NotFound)
        ));
    }
    // phase: nested_symlink_obeys_session_root_and_retains_target
    {
        let (temp, authority, session, root) = granted_root();
        std::fs::create_dir(temp.path().join("child")).unwrap();
        std::fs::write(temp.path().join("sibling"), b"original").unwrap();
        symlink("../sibling", temp.path().join("child/link")).unwrap();
        let child = directory(session.lookup(&root, &name("child")).unwrap());
        let link = file(session.lookup(&child, &name("link")).unwrap());
        assert_eq!(session.read_at(&link, 0, 20).unwrap(), b"original");
        let narrowed = bind(&authority, &child, MOUNT_B);
        let narrowed_link = file(narrowed.lookup(&child, &name("link")).unwrap());
        assert!(matches!(
            narrowed.read_at(&narrowed_link, 0, 20),
            Err(FsError::Denied)
        ));
        assert!(matches!(
            narrowed.read_at(&link, 0, 20),
            Err(FsError::Denied)
        ));
        let overlapping = authority
            .bind(
                files_owner(),
                uuid::Uuid::from_u128(0xc33),
                &[root, child.clone()],
                &[],
            )
            .unwrap();
        let overlapping_link = file(overlapping.lookup(&child, &name("link")).unwrap());
        assert_eq!(
            overlapping.read_at(&overlapping_link, 0, 20).unwrap(),
            b"original"
        );
        std::fs::rename(temp.path().join("sibling"), temp.path().join("held")).unwrap();
        std::fs::write(temp.path().join("sibling"), b"replacement").unwrap();
        assert_eq!(session.read_at(&link, 0, 20).unwrap(), b"original");
        assert_eq!(
            overlapping.read_at(&overlapping_link, 0, 20).unwrap(),
            b"original"
        );
    }
}

#[test]
fn grants_keep_their_descriptor_when_a_path_is_replaced() {
    // phase: renamed_root_does_not_switch_to_replacement
    {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("selected")).unwrap();
        std::fs::write(temp.path().join("selected/old.txt"), b"old").unwrap();
        let authority = NativeGrantAuthority::new();
        let root = admit(&authority, &temp.path().join("selected"));
        let session = bind(&authority, &root, MOUNT_A);
        std::fs::rename(temp.path().join("selected"), temp.path().join("moved")).unwrap();
        std::fs::create_dir(temp.path().join("selected")).unwrap();
        std::fs::write(temp.path().join("selected/attacker.txt"), b"attacker").unwrap();
        let page = session.list(&root, None).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries.first().unwrap().name, name("old.txt"));
        let old = file(session.lookup(&root, &name("old.txt")).unwrap());
        assert_eq!(session.read_at(&old, 0, 100).unwrap(), b"old");
        assert!(matches!(
            session.lookup(&root, &name("attacker.txt")),
            Err(FsError::NotFound)
        ));
        session.check_directory(&root).unwrap();
    }
    // phase: listed_grant_does_not_switch_to_replacement
    {
        let (temp, _authority, session, root) = granted_root();
        std::fs::write(temp.path().join("draft"), b"old").unwrap();
        let grant = file(session.list(&root, None).unwrap().entries.remove(0).grant);
        std::fs::rename(temp.path().join("draft"), temp.path().join("held")).unwrap();
        std::fs::write(temp.path().join("draft"), b"new").unwrap();
        assert_eq!(session.read_at(&grant, 0, 10).unwrap(), b"old");
        assert_eq!(
            session
                .read_at(&file(session.lookup(&root, &name("draft")).unwrap()), 0, 10)
                .unwrap(),
            b"new"
        );
        std::fs::remove_file(temp.path().join("held")).unwrap();
        assert_eq!(session.read_at(&grant, 0, 10).unwrap(), b"old");
    }
    // phase: swapped_parent_uses_derived_descriptor
    {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("root/parent")).unwrap();
        std::fs::create_dir(temp.path().join("outside")).unwrap();
        std::fs::write(temp.path().join("root/parent/inside.txt"), b"inside").unwrap();
        std::fs::write(temp.path().join("outside/secret.txt"), b"secret").unwrap();
        let authority = NativeGrantAuthority::new();
        let root = admit(&authority, &temp.path().join("root"));
        let session = bind(&authority, &root, MOUNT_A);
        let parent = directory(session.lookup(&root, &name("parent")).unwrap());
        std::fs::rename(
            temp.path().join("root/parent"),
            temp.path().join("root/held"),
        )
        .unwrap();
        symlink("../outside", temp.path().join("root/parent")).unwrap();
        assert_eq!(
            session
                .read_at(
                    &file(session.lookup(&parent, &name("inside.txt")).unwrap()),
                    0,
                    20
                )
                .unwrap(),
            b"inside"
        );
        assert!(matches!(
            session.lookup(&parent, &name("secret.txt")),
            Err(FsError::NotFound)
        ));
    }
}

#[test]
fn non_utf8_names_remain_distinct() {
    let (temp, _authority, session, root) = granted_root();
    let first = EntryName::new(OsString::from_vec(b"name-\xfe".to_vec())).unwrap();
    let second = EntryName::new(OsString::from_vec(b"name-\xff".to_vec())).unwrap();
    assert_eq!(first.as_os_str().as_bytes(), b"name-\xfe");
    assert_eq!(second.as_os_str().as_bytes(), b"name-\xff");
    assert_ne!(first, second);
    assert_eq!(HashSet::from([first.clone(), second.clone()]).len(), 2);
    let writes = [
        std::fs::write(temp.path().join(first.as_os_str()), b"first"),
        std::fs::write(temp.path().join(second.as_os_str()), b"second"),
    ];
    // Some macOS backing filesystems accept only valid UTF-8 filenames. This
    // qualification does not weaken EntryName identity or Linux storage checks.
    #[cfg(target_os = "macos")]
    if writes.iter().all(|result| {
        result.as_ref().is_err_and(|error| {
            error.raw_os_error() == Some(rustix::io::Errno::ILSEQ.raw_os_error())
        })
    }) {
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
        assert!(session.list(&root, None).unwrap().entries.is_empty());
        println!("NON_UTF8_STORAGE=UNAVAILABLE_EILSEQ");
        return;
    }
    for result in writes {
        result.unwrap();
    }
    let page = session.list(&root, None).unwrap();
    let names: HashSet<_> = page
        .entries
        .iter()
        .map(|row| row.name.as_os_str().as_bytes().to_vec())
        .collect();
    assert_eq!(
        names,
        HashSet::from([b"name-\xfe".to_vec(), b"name-\xff".to_vec()])
    );
    for row in page.entries {
        let expected = if row.name.as_os_str().as_bytes() == b"name-\xfe" {
            b"first".as_slice()
        } else {
            b"second".as_slice()
        };
        assert_eq!(
            session
                .read_at(&file(session.lookup(&root, &row.name).unwrap()), 0, 20)
                .unwrap(),
            expected
        );
    }
    println!("NON_UTF8_STORAGE=VERIFIED");
}

#[test]
fn directory_pages_are_complete_and_cursors_are_scoped() {
    // phase: missing_is_not_empty
    {
        let (temp, _authority, session, root) = granted_root();
        std::fs::create_dir(temp.path().join("empty")).unwrap();
        let empty = directory(session.lookup(&root, &name("empty")).unwrap());
        assert!(session.list(&empty, None).unwrap().entries.is_empty());
        assert!(matches!(
            session.lookup(&empty, &name("absent")),
            Err(FsError::NotFound)
        ));
    }
    // phase: directory_pages_are_complete_bounded_and_stable
    {
        let (temp, _authority, session, root) = granted_root();
        fill_pages(temp.path());
        let first = session.list(&root, None).unwrap();
        let generation = first.generation;
        assert_eq!(first.entries.len(), 256);
        assert!(first.next.is_some());
        std::fs::write(temp.path().join("later"), b"later").unwrap();
        let mut page = first;
        let mut names = HashSet::new();
        loop {
            assert!(page.entries.len() <= 256);
            assert_eq!(page.generation, generation);
            for row in page.entries {
                assert!(names.insert(row.name));
            }
            let Some(cursor) = page.next else { break };
            page = session.list(&root, Some(cursor)).unwrap();
        }
        assert_eq!(
            names,
            (0..257).map(|i| name(&format!("page-{i:03}"))).collect()
        );
    }
    // phase: page_cursor_is_session_scoped
    {
        let (temp, authority, session, root) = granted_root();
        fill_pages(temp.path());
        let other = bind(&authority, &root, MOUNT_B);
        let cursor = session.list(&root, None).unwrap().next.unwrap();
        assert!(matches!(
            other.list(&root, Some(cursor.clone())),
            Err(FsError::StalePage)
        ));
        std::fs::create_dir(temp.path().join("child")).unwrap();
        let child = directory(session.lookup(&root, &name("child")).unwrap());
        assert!(matches!(
            session.list(&child, Some(cursor.clone())),
            Err(FsError::StalePage)
        ));
        authority.revoke_directory(&root).unwrap();
        assert!(matches!(
            session.list(&root, Some(cursor.clone())),
            Err(FsError::Revoked)
        ));
        session.close();
        assert!(matches!(
            session.list(&root, Some(cursor)),
            Err(FsError::Closed)
        ));
    }
    // phase: page_cursor_cannot_cross_authorities
    {
        let (left_temp, _left_authority, left_session, left_root) = granted_root();
        let (right_temp, _right_authority, right_session, right_root) = granted_root();
        fill_pages(left_temp.path());
        fill_pages(right_temp.path());
        let foreign = left_session.list(&left_root, None).unwrap().next.unwrap();
        let local = right_session.list(&right_root, None).unwrap().next.unwrap();
        assert!(matches!(
            right_session.list(&right_root, Some(foreign)),
            Err(FsError::StalePage)
        ));
        assert_eq!(
            right_session
                .list(&right_root, Some(local))
                .unwrap()
                .entries
                .len(),
            1
        );
    }
}

#[test]
fn closed_sessions_and_lifetimes_are_terminal() {
    // phase: session_close_is_terminal_and_fresh_mount_is_independent
    {
        let (temp, authority, session, root) = granted_root();
        std::fs::write(temp.path().join("alpha.txt"), b"alpha").unwrap();
        let grant = file(session.lookup(&root, &name("alpha.txt")).unwrap());
        let clone = session.clone();
        session.close();
        assert!(matches!(
            clone.read_at(&grant, 0, u32::MAX),
            Err(FsError::Closed)
        ));
        assert!(matches!(
            session.metadata(&GrantedEntry::File(grant)),
            Err(FsError::Closed)
        ));
        assert!(matches!(session.list(&root, None), Err(FsError::Closed)));
        assert!(matches!(
            session.lookup(&root, &name("alpha.txt")),
            Err(FsError::Closed)
        ));
        assert!(matches!(
            session.check_directory(&root),
            Err(FsError::Closed)
        ));
        assert!(matches!(
            session.attach_lifetime(Arc::new(|| true)),
            Err(FsError::Closed)
        ));
        let fresh = bind(&authority, &root, MOUNT_B);
        assert_eq!(fresh.mount_id(), MOUNT_B);
        assert_eq!(fresh.owner(), &files_owner());
        assert_eq!(fresh.list(&root, None).unwrap().entries.len(), 1);
    }
    // phase: attached_lifetime_closes_before_syscall
    {
        let (_temp, _authority, session, root) = granted_root();
        let alive = Arc::new(AtomicBool::new(true));
        let predicate = Arc::clone(&alive);
        session
            .attach_lifetime(Arc::new(move || predicate.load(Ordering::SeqCst)))
            .unwrap();
        session.check_directory(&root).unwrap();
        alive.store(false, Ordering::SeqCst);
        assert!(matches!(session.list(&root, None), Err(FsError::Closed)));
        alive.store(true, Ordering::SeqCst);
        assert!(matches!(
            session.check_directory(&root),
            Err(FsError::Closed)
        ));
    }
}

#[test]
fn reads_and_entry_names_are_bounded_and_validated() {
    // phase: read_at_caps_chunks_and_handles_boundaries
    {
        let (temp, _authority, session, root) = granted_root();
        let bytes = vec![0x5a; MAX_READ_CHUNK as usize + 17];
        std::fs::write(temp.path().join("chunk.bin"), &bytes).unwrap();
        let grant = file(session.lookup(&root, &name("chunk.bin")).unwrap());
        assert_eq!(
            session.read_at(&grant, 0, u32::MAX).unwrap(),
            vec![0x5a; MAX_READ_CHUNK as usize]
        );
        assert_eq!(
            session
                .read_at(&grant, u64::from(MAX_READ_CHUNK), 100)
                .unwrap(),
            vec![0x5a; 17]
        );
        for (offset, len) in [
            (0, 0),
            (u64::from(MAX_READ_CHUNK) + 17, 1),
            (u64::from(MAX_READ_CHUNK) + 30, 10),
        ] {
            assert!(session.read_at(&grant, offset, len).unwrap().is_empty());
        }
    }
    // phase: special_entry_read_is_unsupported
    {
        let (temp, _authority, session, root) = granted_root();
        let _listener =
            std::os::unix::net::UnixListener::bind(temp.path().join("control.sock")).unwrap();
        let row = session.list(&root, None).unwrap().entries.remove(0);
        assert_eq!(row.metadata.kind, EntryKind::Other);
        assert!(matches!(
            session.read_at(&file(row.grant), 0, 10),
            Err(FsError::Unsupported(_))
        ));
    }
    // phase: invalid_entry_names_are_rejected_before_syscall
    {
        for value in ["", ".", "..", "/", "a/b", "a\0b", "a/"] {
            assert!(matches!(
                EntryName::new(value.into()).map_err(FsError::from),
                Err(FsError::InvalidName)
            ));
        }
        assert!(EntryName::new(OsString::from_vec(vec![0xff])).is_ok());
        assert!(EntryName::new("a\\b".into()).is_ok());
    }
}

// Every target and adversarial link remains below this owned TempDir.
fn assert_symlink_directory_requirement(file_target: &str, directory_target: &str) {
    let (temp, _authority, session, root) = granted_root();
    std::fs::write(temp.path().join("data"), b"data").unwrap();
    std::fs::create_dir(temp.path().join("folder")).unwrap();
    for (link, target) in [
        ("file-alias", "data"),
        ("directory-alias", "folder"),
        ("file-slash", "data/"),
        ("directory-slash", "folder/"),
        ("file-dot", "data/."),
        ("directory-dot", "folder/."),
        ("file-link", file_target),
        ("directory-link", directory_target),
    ] {
        symlink(target, temp.path().join(link)).unwrap();
    }
    let directory_link = file(session.lookup(&root, &name("directory-link")).unwrap());
    assert!(matches!(
        session.read_at(&directory_link, 0, 20),
        Err(FsError::Unsupported(_))
    ));
    let native_error = std::fs::metadata(temp.path().join("file-link")).unwrap_err();
    assert_eq!(native_error.kind(), std::io::ErrorKind::NotADirectory);
    let file_link = file(session.lookup(&root, &name("file-link")).unwrap());
    assert!(
        matches!(session.read_at(&file_link, 0, 20), Err(FsError::Io(error)) if error.kind() == std::io::ErrorKind::NotADirectory)
    );
}

#[test]
fn a_symlink_with_a_trailing_directory_component_requires_a_directory() {
    // (file target, directory target): trailing slash, trailing dot, and the
    // nested links that preserve the trailing directory requirement.
    for (file_target, directory_target) in [
        ("data/", "folder/"),
        ("data/.", "folder/."),
        ("file-alias/", "directory-alias/"),
        ("file-alias/.", "directory-alias/."),
        ("file-slash", "directory-slash"),
        ("file-dot", "directory-dot"),
    ] {
        eprintln!("case: {file_target} / {directory_target}");
        assert_symlink_directory_requirement(file_target, directory_target);
    }
}

fn selected_write_file(
    rights: Vec<FsRight>,
) -> (
    tempfile::TempDir,
    NativeGrantAuthority,
    runtime_fs::FsSession,
    FileGrant,
) {
    let temp = tempfile::tempdir().unwrap();
    let selected = temp.path().join("selected");
    std::fs::write(&selected, b"before bytes").unwrap();
    let authority = NativeGrantAuthority::new();
    let grant = authority
        .admit_file(
            std::fs::File::options()
                .read(true)
                .write(true)
                .open(&selected)
                .unwrap(),
            files_owner(),
            rights,
        )
        .unwrap();
    let session = authority
        .bind(files_owner(), MOUNT_A, &[], std::slice::from_ref(&grant))
        .unwrap();
    (temp, authority, session, grant)
}

#[test]
fn selected_regular_file_write_changes_original_bytes_on_same_inode() {
    use std::os::unix::fs::MetadataExt as _;
    let (temp, _authority, session, grant) =
        selected_write_file(vec![FsRight::Read, FsRight::Write]);
    let selected = temp.path().join("selected");
    let before = std::fs::metadata(&selected).unwrap();
    assert_eq!(session.write_at(&grant, 0, b"after!").unwrap(), 6);
    let after = std::fs::metadata(&selected).unwrap();
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!(std::fs::read(&selected).unwrap(), b"after! bytes");
    assert_eq!(session.read_at(&grant, 0, 32).unwrap(), b"after! bytes");
}

#[test]
fn short_selected_file_rewrite_truncates_old_suffix_and_syncs_same_inode() {
    use std::os::unix::fs::MetadataExt as _;
    let (temp, _authority, session, grant) = selected_write_file(vec![FsRight::Write]);
    let selected = temp.path().join("selected");
    let before = std::fs::metadata(&selected).unwrap();
    session.truncate(&grant, 0).unwrap();
    assert_eq!(session.write_at(&grant, 0, b"short").unwrap(), 5);
    session.sync_file(&grant).unwrap();
    let after = std::fs::metadata(&selected).unwrap();
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!(after.len(), 5);
    assert_eq!(std::fs::read(&selected).unwrap(), b"short");
}

#[test]
fn missing_write_right_refuses_write_truncate_and_sync_without_changing_bytes() {
    let (temp, _authority, session, grant) = selected_write_file(vec![FsRight::Read]);
    assert!(matches!(
        session.write_at(&grant, 0, b"changed"),
        Err(FsError::Denied)
    ));
    assert!(matches!(session.truncate(&grant, 0), Err(FsError::Denied)));
    assert!(matches!(session.sync_file(&grant), Err(FsError::Denied)));
    assert_eq!(
        std::fs::read(temp.path().join("selected")).unwrap(),
        b"before bytes"
    );
}

#[test]
fn write_right_never_reopens_read_only_descriptor_to_gain_write_access() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("selected");
    std::fs::write(&path, b"before bytes").unwrap();
    let authority = NativeGrantAuthority::new();
    let grant = authority
        .admit_file(
            std::fs::File::open(&path).unwrap(),
            files_owner(),
            vec![FsRight::Write],
        )
        .unwrap();
    let session = authority
        .bind(files_owner(), MOUNT_A, &[], std::slice::from_ref(&grant))
        .unwrap();
    assert!(matches!(
        session.write_at(&grant, 0, b"changed"),
        Err(FsError::Denied)
    ));
    assert!(matches!(session.truncate(&grant, 0), Err(FsError::Denied)));
    assert!(matches!(session.sync_file(&grant), Err(FsError::Denied)));
    assert_eq!(std::fs::read(&path).unwrap(), b"before bytes");
}

#[test]
fn revoked_closed_and_lost_lifetime_refuse_every_held_file_effect() {
    {
        let (temp, authority, session, grant) = selected_write_file(vec![FsRight::Write]);
        authority.revoke_file(&grant).unwrap();
        assert!(matches!(
            session.write_at(&grant, 0, b"changed"),
            Err(FsError::Revoked)
        ));
        assert!(matches!(session.truncate(&grant, 0), Err(FsError::Revoked)));
        assert!(matches!(session.sync_file(&grant), Err(FsError::Revoked)));
        assert_eq!(
            std::fs::read(temp.path().join("selected")).unwrap(),
            b"before bytes"
        );
    }
    {
        let (temp, _authority, session, grant) = selected_write_file(vec![FsRight::Write]);
        session.close();
        assert!(matches!(
            session.write_at(&grant, 0, b"changed"),
            Err(FsError::Closed)
        ));
        assert!(matches!(session.truncate(&grant, 0), Err(FsError::Closed)));
        assert!(matches!(session.sync_file(&grant), Err(FsError::Closed)));
        assert_eq!(
            std::fs::read(temp.path().join("selected")).unwrap(),
            b"before bytes"
        );
    }
    {
        let (temp, _authority, session, grant) = selected_write_file(vec![FsRight::Write]);
        let alive = Arc::new(AtomicBool::new(true));
        let predicate = Arc::clone(&alive);
        session
            .attach_lifetime(Arc::new(move || predicate.load(Ordering::Acquire)))
            .unwrap();
        alive.store(false, Ordering::Release);
        assert!(matches!(
            session.write_at(&grant, 0, b"changed"),
            Err(FsError::Closed)
        ));
        assert!(matches!(session.truncate(&grant, 0), Err(FsError::Closed)));
        assert!(matches!(session.sync_file(&grant), Err(FsError::Closed)));
        // Terminal: restoring the external predicate cannot revive this session.
        alive.store(true, Ordering::Release);
        assert!(matches!(
            session.write_at(&grant, 0, b"changed"),
            Err(FsError::Closed)
        ));
        assert_eq!(
            std::fs::read(temp.path().join("selected")).unwrap(),
            b"before bytes"
        );
    }
}

#[test]
fn selected_name_replacement_cannot_retarget_write_truncate_or_sync() {
    use std::os::unix::fs::MetadataExt as _;
    let (temp, _authority, session, grant) = selected_write_file(vec![FsRight::Write]);
    let selected = temp.path().join("selected");
    let retained = temp.path().join("retained-original");
    let before = std::fs::metadata(&selected).unwrap();
    std::fs::rename(&selected, &retained).unwrap();
    std::fs::write(&selected, b"replacement untouched").unwrap();
    session.truncate(&grant, 0).unwrap();
    assert_eq!(
        session.write_at(&grant, 0, b"original changed").unwrap(),
        16
    );
    session.sync_file(&grant).unwrap();
    let after = std::fs::metadata(&retained).unwrap();
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!(std::fs::read(&retained).unwrap(), b"original changed");
    assert_eq!(std::fs::read(&selected).unwrap(), b"replacement untouched");
}

#[test]
fn foreign_owner_authority_and_unbound_session_refuse_held_file_effects() {
    let (temp, authority, _session, grant) = selected_write_file(vec![FsRight::Write]);
    let mut wrong = files_owner();
    wrong.workspace = "foreign-room".into();
    assert!(matches!(
        authority.bind(wrong.clone(), MOUNT_B, &[], std::slice::from_ref(&grant)),
        Err(FsError::Denied)
    ));
    let foreign_owner = authority.bind(wrong, MOUNT_B, &[], &[]).unwrap();
    let foreign = NativeGrantAuthority::new();
    let foreign_authority = foreign.bind(files_owner(), MOUNT_B, &[], &[]).unwrap();
    let unbound = authority.bind(files_owner(), MOUNT_B, &[], &[]).unwrap();
    for session in [foreign_owner, foreign_authority, unbound] {
        assert!(matches!(
            session.write_at(&grant, 0, b"changed"),
            Err(FsError::Denied)
        ));
        assert!(matches!(session.truncate(&grant, 0), Err(FsError::Denied)));
        assert!(matches!(session.sync_file(&grant), Err(FsError::Denied)));
    }
    assert_eq!(
        std::fs::read(temp.path().join("selected")).unwrap(),
        b"before bytes"
    );
}

#[test]
fn write_chunks_and_lengths_are_bounded_before_any_file_effect() {
    let (temp, _authority, session, grant) = selected_write_file(vec![FsRight::Write]);
    let oversized = vec![b'x'; runtime_fs::MAX_WRITE_CHUNK as usize + 1];
    assert!(matches!(
        session.write_at(&grant, 0, &oversized),
        Err(FsError::LimitExceeded)
    ));
    assert!(matches!(
        session.write_at(&grant, u64::MAX, b"x"),
        Err(FsError::LimitExceeded)
    ));
    assert!(matches!(
        session.truncate(&grant, u64::MAX),
        Err(FsError::LimitExceeded)
    ));
    assert_eq!(
        std::fs::read(temp.path().join("selected")).unwrap(),
        b"before bytes"
    );
}

#[test]
fn append_descriptor_cannot_redirect_offset_write_to_eof() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("selected");
    std::fs::write(&path, b"before bytes").unwrap();
    let authority = NativeGrantAuthority::new();
    let grant = authority
        .admit_file(
            std::fs::File::options()
                .read(true)
                .append(true)
                .open(&path)
                .unwrap(),
            files_owner(),
            vec![FsRight::Write],
        )
        .unwrap();
    let session = authority
        .bind(files_owner(), MOUNT_A, &[], std::slice::from_ref(&grant))
        .unwrap();
    assert!(matches!(
        session.write_at(&grant, 0, b"changed"),
        Err(FsError::Unsupported(_))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"before bytes");
}

#[test]
fn special_file_descriptor_never_acquires_regular_file_effects() {
    let authority = NativeGrantAuthority::new();
    let grant = authority
        .admit_file(
            std::fs::File::options()
                .read(true)
                .write(true)
                .open("/dev/null")
                .unwrap(),
            files_owner(),
            vec![FsRight::Write],
        )
        .unwrap();
    let session = authority
        .bind(files_owner(), MOUNT_A, &[], std::slice::from_ref(&grant))
        .unwrap();
    assert!(matches!(
        session.write_at(&grant, 0, b"changed"),
        Err(FsError::Unsupported(_))
    ));
    assert!(matches!(
        session.truncate(&grant, 0),
        Err(FsError::Unsupported(_))
    ));
    assert!(matches!(
        session.sync_file(&grant),
        Err(FsError::Unsupported(_))
    ));
}

#[test]
fn explicit_child_access_preserves_raw_names_lineage_and_readonly_lookup() {
    use runtime_fs::NativeFileAccess;
    let disk = tempfile::tempdir().unwrap();
    let raw = OsString::from_vec(vec![b'f', 0xff]);
    let path = disk.path().join(&raw);
    std::fs::write(&path, b"original").unwrap();
    let authority = NativeGrantAuthority::new();
    let owner = files_owner();
    let root = authority
        .admit_directory(
            std::fs::File::open(disk.path()).unwrap(),
            owner.clone(),
            vec![FsRight::Enumerate, FsRight::Read, FsRight::Write],
        )
        .unwrap();
    let session = authority
        .bind(owner, MOUNT_A, std::slice::from_ref(&root), &[])
        .unwrap();
    let name = EntryName::new(raw).unwrap();
    let readonly = file(session.lookup(&root, &name).unwrap());
    assert!(matches!(
        session.write_at(&readonly, 0, b"bad"),
        Err(FsError::Denied)
    ));
    let editable = session
        .lookup_file_for(&root, &name, NativeFileAccess::ReadWrite)
        .unwrap();
    session.validate_read_write(&editable).unwrap();
    assert_eq!(session.read_at(&editable, 0, 8).unwrap(), b"original");
    assert_eq!(session.write_at(&editable, 0, b"edited!!").unwrap(), 8);
    symlink(name.as_os_str(), disk.path().join("link")).unwrap();
    assert!(
        session
            .lookup_file_for(
                &root,
                &EntryName::new("link".into()).unwrap(),
                NativeFileAccess::ReadWrite
            )
            .is_err()
    );
    let writeonly = session
        .lookup_file_for(&root, &name, NativeFileAccess::Write)
        .unwrap();
    assert!(session.validate_read_write(&writeonly).is_err());
    assert_eq!(session.write_at(&writeonly, 0, b"W").unwrap(), 1);
    authority.revoke_directory(&root).unwrap();
    assert!(session.write_at(&editable, 0, b"bad").is_err());
    assert!(
        session
            .lookup_file_for(&root, &name, NativeFileAccess::ReadWrite)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"Wdited!!");
}

#[test]
fn explicit_child_access_does_not_upgrade_readonly_parent_rights() {
    use runtime_fs::NativeFileAccess;
    let disk = tempfile::tempdir().unwrap();
    std::fs::write(disk.path().join("chosen"), b"original").unwrap();
    let authority = NativeGrantAuthority::new();
    let owner = files_owner();
    let root = authority
        .admit_directory(
            std::fs::File::open(disk.path()).unwrap(),
            owner.clone(),
            vec![FsRight::Enumerate, FsRight::Read],
        )
        .unwrap();
    let session = authority
        .bind(owner, MOUNT_A, std::slice::from_ref(&root), &[])
        .unwrap();
    let name = EntryName::new("chosen".into()).unwrap();
    assert!(matches!(
        session.lookup_file_for(&root, &name, NativeFileAccess::Write),
        Err(FsError::Denied)
    ));
    assert!(matches!(
        session.lookup_file_for(&root, &name, NativeFileAccess::ReadWrite),
        Err(FsError::Denied)
    ));
    let selected = session
        .lookup_file_for(&root, &name, NativeFileAccess::Read)
        .unwrap();
    assert_eq!(session.read_at(&selected, 0, 8).unwrap(), b"original");
}

#[test]
#[cfg(target_os = "linux")]
fn raw_readlink_is_metadata_only_preserves_bytes_and_refuses_substitution() {
    let temp = tempfile::tempdir().unwrap();
    let authority = NativeGrantAuthority::new();
    let root = authority
        .admit_directory(
            std::fs::File::open(temp.path()).unwrap(),
            files_owner(),
            vec![FsRight::Enumerate],
        )
        .unwrap();
    let session = bind(&authority, &root, MOUNT_A);
    let raw = OsString::from_vec(b"../missing-\xff".to_vec());
    symlink(&raw, temp.path().join("link")).unwrap();
    let expected = session.stat_child(&root, &name("link")).unwrap().identity;
    assert_eq!(
        session
            .readlink_child(&root, &name("link"), expected)
            .unwrap(),
        raw.as_bytes()
    );
    // Link bytes are observable without resolving or gaining Read on its target.
    let linked = file(session.lookup(&root, &name("link")).unwrap());
    assert!(matches!(
        session.read_at(&linked, 0, 10),
        Err(FsError::Denied)
    ));
    symlink("/var/home/bob/private", temp.path().join("absolute")).unwrap();
    let absolute = session
        .stat_child(&root, &name("absolute"))
        .unwrap()
        .identity;
    assert_eq!(
        session
            .readlink_child(&root, &name("absolute"), absolute)
            .unwrap(),
        b"/var/home/bob/private"
    );
    symlink("replacement", temp.path().join("new-link")).unwrap();
    std::fs::rename(temp.path().join("new-link"), temp.path().join("link")).unwrap();
    assert!(matches!(
        session.readlink_child(&root, &name("link"), expected),
        Err(FsError::Busy)
    ));
    authority.revoke_directory(&root).unwrap();
    assert!(
        session
            .readlink_child(&root, &name("absolute"), absolute)
            .is_err()
    );
}
