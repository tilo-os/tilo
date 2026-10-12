//! Real native directory capability + existing connected RPC, with same-name
//! replacement refusing truncate and existing held handles preserving old bytes.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    reason = "owned native connected capability assertions"
)]
use runtime_fs::{
    EntryName, FsOwner, FsRight, NativeGrantAuthority, OpenMode,
    directory::DirectoryWorkspaceBackend,
    directory_connection::{DirectoryConnection, ROOT_DIRECTORY},
    workspace_server::serve_workspace_connected,
};
use std::{
    fs::File,
    os::unix::{fs::MetadataExt, net::UnixStream},
    sync::Arc,
};
#[test]
fn connected_directory_pins_open_identity_and_observes_real_retirement() {
    let disk = tempfile::tempdir().unwrap();
    let path = disk.path().join("original");
    std::fs::write(&path, b"old bytes").unwrap();
    let owner = FsOwner {
        workspace: "native-directory-fixture".into(),
        app_id: "so.tilo.file-explorer".into(),
    };
    let authority = NativeGrantAuthority::new();
    let root = authority
        .admit_directory(
            File::open(disk.path()).unwrap(),
            owner.clone(),
            vec![FsRight::Enumerate, FsRight::Read, FsRight::Write],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            std::slice::from_ref(&root),
            &[],
        )
        .unwrap();
    let backend = DirectoryWorkspaceBackend::new(session, root.clone()).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (server_stream, stream) = UnixStream::pair().unwrap();
    let server =
        serve_workspace_connected(server_stream, Arc::new(backend), runtime.handle().clone())
            .unwrap();
    let source = DirectoryConnection::connected(stream).unwrap();
    let name = EntryName::new("original".into()).unwrap();
    assert!(
        source
            .stat(ROOT_DIRECTORY, &EntryName::new("missing".into()).unwrap())
            .unwrap()
            .is_none()
    );
    let observed = source.stat(ROOT_DIRECTORY, &name).unwrap().unwrap();
    let pin = source
        .pin_metadata(ROOT_DIRECTORY, &name, observed.identity)
        .unwrap();
    assert_eq!(source.metadata_pin(pin).unwrap().file_type, libc::S_IFREG);
    let old = source
        .open_expected(
            ROOT_DIRECTORY,
            &name,
            OpenMode::ReadWriteExisting,
            observed.identity,
        )
        .unwrap();
    std::fs::rename(&path, disk.path().join("held")).unwrap();
    std::fs::write(&path, b"new unrelated bytes").unwrap();
    assert_eq!(
        source.metadata_pin(pin).unwrap().identity,
        observed.identity
    );
    assert_eq!(source.metadata_pin(pin).unwrap().metadata.size, 9);
    let replaced = source.stat(ROOT_DIRECTORY, &name).unwrap().unwrap();
    assert_ne!(observed.identity, replaced.identity);
    assert_eq!(source.read(old, 0, 9).unwrap(), b"old bytes");
    assert_eq!(source.write(old, 0, b"OLD").unwrap(), 3);
    assert_eq!(
        std::fs::read(disk.path().join("held")).unwrap(),
        b"OLD bytes"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"new unrelated bytes");
    let facts = std::fs::metadata(&path).unwrap();
    assert_eq!(replaced.identity, [facts.dev(), facts.ino()]);
    source.release_pin(pin).unwrap();
    // A stale inode observation is refused before actual Write truncation.
    assert!(
        source
            .open_expected(ROOT_DIRECTORY, &name, OpenMode::Write, observed.identity)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"new unrelated bytes");
    assert_eq!(
        source.read(old, 0, 3).unwrap(),
        b"OLD",
        "per-entry native refusal must preserve the independent held handle"
    );
    authority.revoke_directory(&root).unwrap();
    assert!(
        source.read(old, 0, 1).is_err(),
        "actual native ancestor revocation remains terminal"
    );
    assert!(
        source.retire().is_err(),
        "actual authority failure is not clean retirement"
    );
    server.retire().unwrap();
    drop(runtime);
}
#[test]
fn connected_directory_clean_retirement_joins_transport() {
    let disk = tempfile::tempdir().unwrap();
    let owner = FsOwner {
        workspace: "native-directory-fixture".into(),
        app_id: "so.tilo.file-explorer".into(),
    };
    let authority = NativeGrantAuthority::new();
    let root = authority
        .admit_directory(
            File::open(disk.path()).unwrap(),
            owner.clone(),
            vec![FsRight::Enumerate, FsRight::Read],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            std::slice::from_ref(&root),
            &[],
        )
        .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (server_stream, stream) = UnixStream::pair().unwrap();
    let server = serve_workspace_connected(
        server_stream,
        Arc::new(DirectoryWorkspaceBackend::new(session, root).unwrap()),
        runtime.handle().clone(),
    )
    .unwrap();
    let source = DirectoryConnection::connected(stream).unwrap();
    assert!(source.list(ROOT_DIRECTORY, 0).unwrap().entries.is_empty());
    source.retire().unwrap();
    server.retire().unwrap();
    drop(runtime);
}

#[test]
fn connected_metadata_pin_does_not_upgrade_enumeration_to_read() {
    let disk = tempfile::tempdir().unwrap();
    let path = disk.path().join("private");
    std::fs::write(&path, b"private bytes").unwrap();
    let owner = FsOwner {
        workspace: "enumeration-only".into(),
        app_id: "so.tilo.file-explorer".into(),
    };
    let authority = NativeGrantAuthority::new();
    let root = authority
        .admit_directory(
            File::open(disk.path()).unwrap(),
            owner.clone(),
            vec![FsRight::Enumerate],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            std::slice::from_ref(&root),
            &[],
        )
        .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (server_stream, stream) = UnixStream::pair().unwrap();
    let server = serve_workspace_connected(
        server_stream,
        Arc::new(DirectoryWorkspaceBackend::new(session, root).unwrap()),
        runtime.handle().clone(),
    )
    .unwrap();
    let source = DirectoryConnection::connected(stream).unwrap();
    let name = EntryName::new("private".into()).unwrap();
    let stat = source.stat(ROOT_DIRECTORY, &name).unwrap().unwrap();
    let pin = source
        .pin_metadata(ROOT_DIRECTORY, &name, stat.identity)
        .unwrap();
    assert_eq!(source.metadata_pin(pin).unwrap().metadata.size, 13);
    source.release_pin(pin).unwrap();
    assert!(
        source
            .open_expected(ROOT_DIRECTORY, &name, OpenMode::Read, stat.identity)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"private bytes");
    assert!(
        source.metadata(ROOT_DIRECTORY).is_ok(),
        "refused bytes do not close enumeration scope"
    );
    source.retire().unwrap();
    server.retire().unwrap();
    drop(runtime);
}

#[test]
fn independent_connected_cursors_stream_and_read_more_than_1024_files() {
    let disk = tempfile::tempdir().unwrap();
    for index in 0..1500 {
        std::fs::write(
            disk.path().join(format!("entry-{index:04}")),
            format!("contents-{index:04}"),
        )
        .unwrap();
    }
    let owner = FsOwner {
        workspace: "large-held-directory".into(),
        app_id: "so.tilo.file-explorer".into(),
    };
    let authority = NativeGrantAuthority::new();
    let root = authority
        .admit_directory(
            File::open(disk.path()).unwrap(),
            owner.clone(),
            vec![FsRight::Enumerate, FsRight::Read],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            std::slice::from_ref(&root),
            &[],
        )
        .unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (server_stream, stream) = UnixStream::pair().unwrap();
    let server = serve_workspace_connected(
        server_stream,
        Arc::new(DirectoryWorkspaceBackend::new(session, root.clone()).unwrap()),
        runtime.handle().clone(),
    )
    .unwrap();
    let source = DirectoryConnection::connected(stream).unwrap();
    let first = source.open_cursor(ROOT_DIRECTORY).unwrap();
    let second = source.open_cursor(ROOT_DIRECTORY).unwrap();
    let first_page = source.cursor_page(first, 0).unwrap();
    let second_page = source.cursor_page(second, 0).unwrap();
    assert_eq!(
        first_page
            .entries
            .iter()
            .map(|r| r.name.clone())
            .collect::<Vec<_>>(),
        second_page
            .entries
            .iter()
            .map(|r| r.name.clone())
            .collect::<Vec<_>>(),
        "independent cursor OFDs start independently"
    );
    assert_eq!(first_page.entries.len(), 128);
    assert!(!first_page.done);
    let stale = source.cursor_page(second, 0).unwrap_err();
    assert!(runtime_fs::native_reply::is_native_refusal(&stale));
    assert!(
        source.metadata(ROOT_DIRECTORY).is_ok(),
        "per-cursor stale position must not retire Home"
    );
    source.release_cursor(second).unwrap();
    let mut page = first_page;
    let mut seen = std::collections::HashSet::new();
    loop {
        assert!(page.entries.len() <= 128);
        for row in page.entries {
            let stat = row.stat.expect("actual owned rows must be observed");
            assert!(
                seen.insert(row.name.clone()),
                "unmodified directory should enumerate each row once"
            );
            let handle = source
                .open_expected(ROOT_DIRECTORY, &row.name, OpenMode::Read, stat.identity)
                .unwrap();
            let name = row.name.as_os_str().to_str().unwrap();
            let expected = name.replace("entry-", "contents-");
            assert_eq!(source.read(handle, 0, 64).unwrap(), expected.as_bytes());
            source.release(handle).unwrap();
        }
        if page.done {
            break;
        }
        page = source.cursor_page(first, page.next_sequence).unwrap();
    }
    assert_eq!(seen.len(), 1500);
    source.release_cursor(first).unwrap();
    let mutating = source.open_cursor(ROOT_DIRECTORY).unwrap();
    let primed = source.cursor_page(mutating, 0).unwrap();
    assert_eq!(primed.entries.len(), 128);
    assert!(!primed.done);
    // Prime the actual independent native readDir buffer, then remove all names
    // without assuming its order. Buffered names must be absent, not denied rows.
    for name in &seen {
        std::fs::remove_file(disk.path().join(name.as_os_str())).unwrap();
    }
    let mut sequence = primed.next_sequence;
    let mut reached_eof = false;
    for _ in 0..16 {
        let page = source.cursor_page(mutating, sequence).unwrap();
        assert!(
            page.entries.is_empty(),
            "vanished names must not become denial rows"
        );
        assert_eq!(page.next_sequence, sequence + 1);
        sequence = page.next_sequence;
        if page.done {
            reached_eof = true;
            break;
        }
    }
    assert!(
        reached_eof,
        "actual finite directory must reach observed EOF"
    );
    assert!(source.metadata(ROOT_DIRECTORY).is_ok());
    source.release_cursor(mutating).unwrap();
    let resident: Vec<_> = (0..64)
        .map(|_| source.open_cursor(ROOT_DIRECTORY).unwrap())
        .collect();
    let limit = source.open_cursor(ROOT_DIRECTORY).unwrap_err();
    assert!(runtime_fs::native_reply::is_operation_refusal(&limit));
    assert!(!runtime_fs::native_reply::is_native_refusal(&limit));
    assert!(source.metadata(ROOT_DIRECTORY).is_ok());
    for cursor in resident {
        source.release_cursor(cursor).unwrap();
    }
    let revoked = source.open_cursor(ROOT_DIRECTORY).unwrap();
    authority.revoke_directory(&root).unwrap();
    assert!(
        source.cursor_page(revoked, 0).is_err(),
        "actual ancestor revocation remains terminal"
    );
    assert!(source.metadata(ROOT_DIRECTORY).is_err());
    assert!(source.retire().is_err());
    server.retire().unwrap();
    drop(runtime);
}

#[test]
fn connected_statfs_observes_actual_source_filesystem_under_enumerate_only() {
    let disk = tempfile::tempdir().unwrap();
    std::fs::write(disk.path().join("file"), b"ungranted bytes").unwrap();
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let raw_target = std::ffi::OsString::from_vec(b"../missing-\xff".to_vec());
    std::os::unix::fs::symlink(&raw_target, disk.path().join("link")).unwrap();
    let descriptor = File::open(disk.path()).unwrap();
    let owner = FsOwner {
        workspace: "statfs-source-fixture".into(),
        app_id: "so.tilo.file-explorer".into(),
    };
    let authority = NativeGrantAuthority::new();
    let root = authority
        .admit_directory(
            descriptor.try_clone().unwrap(),
            owner.clone(),
            vec![FsRight::Enumerate],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            std::slice::from_ref(&root),
            &[],
        )
        .unwrap();
    let native = session.directory_statfs(&root).unwrap();
    let actual = rustix::fs::fstatvfs(&descriptor).unwrap();
    assert_eq!(native.blocks, actual.f_blocks);
    assert_eq!(native.block_size, u32::try_from(actual.f_bsize).unwrap());
    assert_eq!(
        native.fragment_size,
        u32::try_from(actual.f_frsize).unwrap()
    );
    assert_eq!(native.name_max, u32::try_from(actual.f_namemax).unwrap());
    assert_eq!(native.files, actual.f_files);
    assert_eq!(
        native.read_only,
        actual
            .f_flag
            .contains(rustix::fs::StatVfsMountFlags::RDONLY)
    );
    assert!(
        native.blocks > 0,
        "fixture must observe actual filesystem capacity"
    );
    let backend = DirectoryWorkspaceBackend::new(session, root.clone()).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (server_stream, stream) = UnixStream::pair().unwrap();
    let server =
        serve_workspace_connected(server_stream, Arc::new(backend), runtime.handle().clone())
            .unwrap();
    let source = DirectoryConnection::connected(stream).unwrap();
    let projected = source.statfs(ROOT_DIRECTORY).unwrap();
    projected.validate().unwrap();
    assert_eq!(projected.blocks, native.blocks);
    assert_eq!(projected.block_size, native.block_size);
    assert_eq!(projected.fragment_size, native.fragment_size);
    assert_eq!(projected.name_max, native.name_max);
    assert_eq!(projected.files, native.files);
    let name = EntryName::new("file".into()).unwrap();
    assert!(
        source.open(ROOT_DIRECTORY, &name, OpenMode::Read).is_err(),
        "capacity metadata cannot grant byte Read"
    );
    assert!(
        source.statfs(ROOT_DIRECTORY).is_ok(),
        "one file refusal keeps authentic metadata scope live"
    );
    let link = EntryName::new("link".into()).unwrap();
    let identity = source
        .stat(ROOT_DIRECTORY, &link)
        .unwrap()
        .unwrap()
        .identity;
    assert_eq!(
        source.readlink(ROOT_DIRECTORY, &link, identity).unwrap(),
        raw_target.as_bytes()
    );
    assert!(
        source.open(ROOT_DIRECTORY, &link, OpenMode::Read).is_err(),
        "link metadata grants no target byte access"
    );
    std::os::unix::fs::symlink("replacement", disk.path().join("replacement")).unwrap();
    std::fs::rename(disk.path().join("replacement"), disk.path().join("link")).unwrap();
    assert!(
        source.readlink(ROOT_DIRECTORY, &link, identity).is_err(),
        "RPC refuses replaced link inode"
    );
    let replacement = source
        .stat(ROOT_DIRECTORY, &link)
        .unwrap()
        .unwrap()
        .identity;
    assert_eq!(
        source.readlink(ROOT_DIRECTORY, &link, replacement).unwrap(),
        b"replacement"
    );
    authority.revoke_directory(&root).unwrap();
    assert!(source.readlink(ROOT_DIRECTORY, &link, replacement).is_err());
    assert!(source.statfs(ROOT_DIRECTORY).is_err());
    assert!(
        source.retire().is_err(),
        "revoked scope must not claim clean retirement"
    );
    server.retire().unwrap();
}

#[test]
fn connected_write_existing_preserves_bytes_and_uses_only_real_write_rights() {
    let disk = tempfile::tempdir().unwrap();
    let path = disk.path().join("target");
    std::fs::write(&path, b"abcdefghij").unwrap();
    let owner = FsOwner {
        workspace: "write-existing-fixture".into(),
        app_id: "so.tilo.editor".into(),
    };
    let authority = NativeGrantAuthority::new();
    // Deliberately no Read right: write-only permission cannot grow into Read.
    let root = authority
        .admit_directory(
            File::open(disk.path()).unwrap(),
            owner.clone(),
            vec![FsRight::Enumerate, FsRight::Write],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            std::slice::from_ref(&root),
            &[],
        )
        .unwrap();
    let backend = DirectoryWorkspaceBackend::new(session, root.clone()).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (server_stream, stream) = UnixStream::pair().unwrap();
    let server =
        serve_workspace_connected(server_stream, Arc::new(backend), runtime.handle().clone())
            .unwrap();
    let source = DirectoryConnection::connected(stream).unwrap();
    let name = EntryName::new("target".into()).unwrap();
    let observed = source.stat(ROOT_DIRECTORY, &name).unwrap().unwrap();
    let handle = source
        .open_expected(
            ROOT_DIRECTORY,
            &name,
            OpenMode::WriteExisting,
            observed.identity,
        )
        .unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"abcdefghij",
        "opening the permission probe must not truncate"
    );
    assert_eq!(source.write(handle, 2, b"XY").unwrap(), 2);
    source.flush(handle).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"abXYefghij");
    assert!(
        source.read(handle, 0, 1).is_err(),
        "write-only handle must deny actual connected reads"
    );
    authority.revoke_directory(&root).unwrap();
    assert!(source.write(handle, 0, b"Z").is_err());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"abXYefghij",
        "revocation must precede any subsequent write effect"
    );
    assert!(source.retire().is_err());
    server.retire().unwrap();
    drop(runtime);
}

#[test]
fn connected_readonly_source_refuses_write_existing_without_changing_bytes() {
    let disk = tempfile::tempdir().unwrap();
    let path = disk.path().join("target");
    std::fs::write(&path, b"keep original").unwrap();
    let owner = FsOwner {
        workspace: "readonly-existing-fixture".into(),
        app_id: "so.tilo.reader".into(),
    };
    let authority = NativeGrantAuthority::new();
    let root = authority
        .admit_directory(
            File::open(disk.path()).unwrap(),
            owner.clone(),
            vec![FsRight::Enumerate, FsRight::Read],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            std::slice::from_ref(&root),
            &[],
        )
        .unwrap();
    let backend = DirectoryWorkspaceBackend::new(session, root).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (server_stream, stream) = UnixStream::pair().unwrap();
    let server =
        serve_workspace_connected(server_stream, Arc::new(backend), runtime.handle().clone())
            .unwrap();
    let source = DirectoryConnection::connected(stream).unwrap();
    let name = EntryName::new("target".into()).unwrap();
    let observed = source.stat(ROOT_DIRECTORY, &name).unwrap().unwrap();
    assert!(
        source
            .open_expected(
                ROOT_DIRECTORY,
                &name,
                OpenMode::WriteExisting,
                observed.identity
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"keep original");
    source.retire().unwrap();
    server.retire().unwrap();
    drop(runtime);
}

#[test]
fn actual_readonly_description_cannot_satisfy_a_declared_write_probe() {
    let disk = tempfile::tempdir().unwrap();
    let path = disk.path().join("target");
    std::fs::write(&path, b"original bytes").unwrap();
    let owner = FsOwner {
        workspace: "readonly-description".into(),
        app_id: "so.tilo.editor".into(),
    };
    let authority = NativeGrantAuthority::new();
    let file = authority
        .admit_file(
            File::open(&path).unwrap(),
            owner.clone(),
            vec![FsRight::Write],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            &[],
            std::slice::from_ref(&file),
        )
        .unwrap();
    assert!(
        session.validate_write(&file).is_err(),
        "a declared right does not change the actual O_RDONLY description"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"original bytes");
}

#[test]
fn connected_directory_sync_uses_held_metadata_grant_and_refuses_revocation() {
    let disk = tempfile::tempdir().unwrap();
    let owner = FsOwner {
        workspace: "directory-sync-fixture".into(),
        app_id: "so.tilo.file-explorer".into(),
    };
    let authority = NativeGrantAuthority::new();
    let root = authority
        .admit_directory(
            File::open(disk.path()).unwrap(),
            owner.clone(),
            vec![FsRight::Enumerate],
        )
        .unwrap();
    let session = authority
        .bind(
            owner,
            uuid::Uuid::new_v4(),
            std::slice::from_ref(&root),
            &[],
        )
        .unwrap();
    // The genuine held descriptor syscall and connected capability agree;
    // there is no byte Read/Write grant in this fixture.
    session.sync_directory(&root).unwrap();
    let backend = DirectoryWorkspaceBackend::new(session, root.clone()).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (server_stream, stream) = UnixStream::pair().unwrap();
    let server =
        serve_workspace_connected(server_stream, Arc::new(backend), runtime.handle().clone())
            .unwrap();
    let source = DirectoryConnection::connected(stream).unwrap();
    source.sync_directory(ROOT_DIRECTORY).unwrap();
    let temporary = EntryName::new("save-temporary".into()).unwrap();
    let victim = EntryName::new("save-victim".into()).unwrap();
    std::fs::write(disk.path().join("save-temporary"), b"new").unwrap();
    std::fs::write(disk.path().join("save-victim"), b"old").unwrap();
    assert!(
        source
            .rename_replace(ROOT_DIRECTORY, &temporary, &victim)
            .is_err(),
        "no actual signed replacing coordinator was enrolled"
    );
    assert_eq!(
        std::fs::read(disk.path().join("save-temporary")).unwrap(),
        b"new"
    );
    assert_eq!(
        std::fs::read(disk.path().join("save-victim")).unwrap(),
        b"old"
    );
    source.sync_directory(ROOT_DIRECTORY).unwrap();
    authority.revoke_directory(&root).unwrap();
    assert!(source.sync_directory(ROOT_DIRECTORY).is_err());
    drop(source);
    // Scope revocation may yield RPC error, but retirement must actually finish.
    let _ = server.retire();
    runtime.shutdown_timeout(std::time::Duration::from_secs(10));
}
