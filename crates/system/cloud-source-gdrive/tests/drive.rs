//! Drive-specific behaviour beyond the conformance suite, against the fake.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::arithmetic_side_effects,
    reason = "tests"
)]

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use cloud_source::tree::Tree;
use cloud_source::{
    Change, CloudSource, Credential, Cursor, Error, ItemId, ItemKind, StaticCredential,
    UploadTarget, Wait,
};
use cloud_source_gdrive::fake::{FakeDrive, FakeFault, REMOTE_TOKEN, SOURCE_TOKEN};
use cloud_source_gdrive::{DriveConfig, GDrive, UPLOAD_QUANTUM};
use zeroize::Zeroizing;

fn drive(config: DriveConfig, token: &str) -> GDrive {
    GDrive::new(
        config,
        Arc::new(StaticCredential(Credential::Bearer {
            token: Zeroizing::new(token.to_owned()),
            expires: None,
        })),
    )
    .unwrap()
}

/// A source on the whole of a fresh fake My Drive.
fn setup() -> (FakeDrive, GDrive) {
    let server = FakeDrive::start();
    let source = drive(server.config(), SOURCE_TOKEN);
    (server, source)
}

fn read_all(source: &GDrive, id: &ItemId) -> Vec<u8> {
    let mut body = Vec::new();
    source
        .read(id, None, None)
        .unwrap()
        .body
        .read_to_end(&mut body)
        .unwrap();
    body
}

fn upload(
    source: &GDrive,
    target: UploadTarget,
    bytes: &[u8],
) -> cloud_source::Result<cloud_source::Item> {
    let owned = bytes.to_vec();
    cloud_source::upload(source, target, bytes.len() as u64, move |offset| {
        Ok(Box::new(std::io::Cursor::new(
            owned[offset as usize..].to_vec(),
        )))
    })
}

fn drain(source: &GDrive, cursor: Option<&Cursor>) -> (Vec<Change>, Cursor) {
    let mut page = source.changes(cursor).unwrap();
    let mut changes = std::mem::take(&mut page.changes);
    while page.has_more {
        page = source.changes(Some(&page.cursor)).unwrap();
        changes.append(&mut page.changes);
    }
    (changes, page.cursor)
}

#[test]
fn duplicate_names_and_multiple_parents_project_through_the_tree() {
    let (server, source) = setup();
    let root = server.root_id();
    let work = source.create_folder(&source.root(), "Work").unwrap();
    let a = server.put_file(&root, "Report.pdf", b"one");
    let b = server.put_file(&root, "Report.pdf", b"two");
    server.add_parent(&a, work.id.as_str());

    let listed = source.list(&source.root()).unwrap();
    let reports: Vec<_> = listed.iter().filter(|i| i.name == "Report.pdf").collect();
    assert_eq!(reports.len(), 2, "Drive keeps both same-named files");
    let shared = reports.iter().find(|i| i.id.as_str() == a).unwrap();
    assert!(
        shared.parents.contains(&ItemId::new("root")),
        "the real root id maps to the alias"
    );
    assert!(shared.parents.contains(&work.id));

    let mut tree = Tree::new(source.root());
    let (changes, _) = drain(&source, None);
    for change in changes {
        if let Change::Upsert(item) = change {
            tree.upsert(item);
        }
    }
    let names: Vec<String> = tree
        .children(&source.root())
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, ["Report (2).pdf", "Report.pdf", "Work"]);
    let mut paths = tree.paths(&ItemId::new(a.clone()));
    paths.sort();
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert!(paths.iter().any(|p| p == &["Work", "Report.pdf"]));
    assert_ne!(a, b);
}

#[test]
fn native_documents_export_and_refuse_media() {
    let (server, source) = setup();
    let id = server.put_native("root", "Budget", "application/vnd.google-apps.spreadsheet");
    let item = source.stat(&ItemId::new(id.clone())).unwrap();
    let ItemKind::Native(native) = &item.kind else {
        panic!("{item:?}")
    };
    assert_eq!(native.link_extension, "gsheet");
    assert!(native.web_url.as_deref().unwrap().contains(&id));
    assert!(matches!(
        source.read(&item.id, None, None),
        Err(Error::Unsupported(_))
    ));
    let xlsx = native
        .exports
        .iter()
        .find(|f| f.extension == "xlsx")
        .unwrap();
    let mut body = Vec::new();
    let download = source.export(&item.id, xlsx).unwrap();
    assert_eq!(download.total, Some(download.range.end));
    download.body.take(1 << 20).read_to_end(&mut body).unwrap();
    assert!(String::from_utf8(body).unwrap().contains(&xlsx.mime));
}

#[test]
fn stale_preconditions_are_conflicts_even_without_atomic_writes() {
    let (server, source) = setup();
    let item = upload(
        &source,
        UploadTarget::Create {
            parent: source.root(),
            name: "doc.txt".into(),
        },
        b"base",
    )
    .unwrap();
    server.write_file(item.id.as_str(), b"theirs");
    let stale = upload(
        &source,
        UploadTarget::Replace {
            id: item.id.clone(),
            expect: item.revision.clone(),
        },
        b"mine",
    );
    assert!(
        matches!(stale, Err(Error::Conflict { current: Some(_) })),
        "{stale:?}"
    );
    assert_eq!(read_all(&source, &item.id), b"theirs");

    let current = source.stat(&item.id).unwrap();
    assert!(matches!(
        source.move_item(&item.id, &source.root(), "x.txt", item.revision.as_ref()),
        Err(Error::Conflict { .. })
    ));
    assert!(matches!(
        source.delete(&item.id, item.revision.as_ref()),
        Err(Error::Conflict { .. })
    ));
    source
        .move_item(
            &item.id,
            &source.root(),
            "renamed.txt",
            current.revision.as_ref(),
        )
        .unwrap();
    source.delete(&item.id, current.revision.as_ref()).unwrap();
}

#[test]
fn a_write_inside_the_commit_window_is_detected_and_pinned() {
    let (server, source) = setup();
    let item = upload(
        &source,
        UploadTarget::Create {
            parent: source.root(),
            name: "race.txt".into(),
        },
        b"base",
    )
    .unwrap();
    server.race_next_commit(item.id.as_str(), b"theirs, just in time");
    let result = upload(
        &source,
        UploadTarget::Replace {
            id: item.id.clone(),
            expect: item.revision.clone(),
        },
        b"mine",
    );
    assert!(matches!(result, Err(Error::Conflict { .. })), "{result:?}");
    let revisions = server.revisions(item.id.as_str());
    assert_eq!(revisions.len(), 3);
    assert!(revisions[1].1, "the overwritten revision is kept forever");
    assert!(!revisions[0].1 && !revisions[2].1);
}

#[test]
fn the_commit_request_carries_at_most_one_quantum() {
    let (server, source) = setup();
    let chunk = source.capabilities().upload_chunk;
    assert_eq!(chunk, UPLOAD_QUANTUM);
    let item = upload(
        &source,
        UploadTarget::Create {
            parent: source.root(),
            name: "big.bin".into(),
        },
        &[1_u8; 10],
    )
    .unwrap();
    // One call with the whole body as a single final chunk: it is split so
    // the precondition is checked right before the last quantum.
    let bytes = vec![7_u8; (UPLOAD_QUANTUM * 2 + 5) as usize];
    let mut session = source
        .begin_upload(
            UploadTarget::Replace {
                id: item.id.clone(),
                expect: item.revision.clone(),
            },
            bytes.len() as u64,
        )
        .unwrap();
    let before = server.stats().upload_puts;
    session
        .write_chunk(
            Box::new(std::io::Cursor::new(bytes.clone())),
            bytes.len() as u64,
        )
        .unwrap();
    assert_eq!(server.stats().upload_puts - before, 2);
    session.finish().unwrap();
    assert_eq!(server.bytes(item.id.as_str()).unwrap(), bytes);
}

#[test]
fn partially_committed_chunks_continue_from_the_server_offset() {
    let (server, source) = setup();
    let size = UPLOAD_QUANTUM * 2;
    let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let mut session = source
        .begin_upload(
            UploadTarget::Create {
                parent: source.root(),
                name: "odd.bin".into(),
            },
            size,
        )
        .unwrap();
    // A non-final chunk that is not a whole number of quanta: Drive keeps
    // only the whole quanta.
    let odd = UPLOAD_QUANTUM + 1000;
    session
        .write_chunk(
            Box::new(std::io::Cursor::new(bytes[..odd as usize].to_vec())),
            odd,
        )
        .unwrap();
    assert_eq!(session.offset(), UPLOAD_QUANTUM);
    let state = session.state().unwrap();
    drop(session);
    let mut resumed = source.resume_upload(&state).unwrap();
    assert_eq!(resumed.offset(), UPLOAD_QUANTUM);
    let owned = bytes.clone();
    cloud_source::drive(resumed.as_mut(), size, UPLOAD_QUANTUM, &mut |offset| {
        Ok(Box::new(std::io::Cursor::new(
            owned[offset as usize..].to_vec(),
        )))
    })
    .unwrap();
    let item = resumed.finish().unwrap();
    assert_eq!(server.bytes(item.id.as_str()).unwrap(), bytes);
}

#[test]
fn empty_files_upload_and_aborted_sessions_are_cancelled() {
    let (_server, source) = setup();
    let empty = upload(
        &source,
        UploadTarget::Create {
            parent: source.root(),
            name: "empty".into(),
        },
        b"",
    )
    .unwrap();
    assert_eq!(empty.size, Some(0));
    assert!(empty.revision.is_some());
    let session = source
        .begin_upload(
            UploadTarget::Create {
                parent: source.root(),
                name: "never".into(),
            },
            10,
        )
        .unwrap();
    let state = session.state().unwrap();
    session.abort().unwrap();
    assert!(matches!(
        source.resume_upload(&state),
        Err(Error::SessionExpired)
    ));
}

#[test]
fn trash_is_the_default_and_permanent_delete_is_configurable() {
    let (server, source) = setup();
    let folder = source.create_folder(&source.root(), "Old").unwrap();
    let inner = server.put_file(folder.id.as_str(), "a.txt", b"a");
    source.delete(&folder.id, None).unwrap();
    assert_eq!(server.is_trashed(folder.id.as_str()), Some(true));
    assert_eq!(
        server.is_trashed(&inner),
        Some(true),
        "contents are trashed with it"
    );
    assert!(matches!(source.stat(&folder.id), Err(Error::NotFound)));
    assert!(source.list(&source.root()).unwrap().is_empty());

    let permanent = drive(
        DriveConfig {
            trash: false,
            ..server.config()
        },
        SOURCE_TOKEN,
    );
    assert!(!permanent.capabilities().trash);
    let gone = server.put_file("root", "gone.txt", b"g");
    permanent.delete(&ItemId::new(gone.clone()), None).unwrap();
    assert_eq!(server.is_trashed(&gone), None);
}

#[test]
fn share_links_quota_and_revisions() {
    let (server, source) = setup();
    let item = upload(
        &source,
        UploadTarget::Create {
            parent: source.root(),
            name: "notes.txt".into(),
        },
        b"first",
    )
    .unwrap();
    let link = source.share_link(&item.id).unwrap();
    assert!(link.contains(item.id.as_str()));
    let permissions = server.permissions(item.id.as_str());
    assert_eq!(permissions[0]["type"], "anyone");
    assert_eq!(permissions[0]["role"], "reader");

    let quota = source.quota().unwrap();
    assert_eq!(quota.total, Some(16_106_127_360));
    assert_eq!(quota.used, 5_368_709_120);

    let second = upload(
        &source,
        UploadTarget::Replace {
            id: item.id.clone(),
            expect: item.revision.clone(),
        },
        b"second!",
    )
    .unwrap();
    let history = source.revisions(&item.id).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[0].revision,
        second.revision.unwrap(),
        "newest first"
    );
    assert_eq!(history[1].size, Some(5));
    assert_eq!(history[1].author.as_deref(), Some("Ada Lovelace"));
    let restored = source
        .restore_revision(&item.id, &history[1].revision)
        .unwrap();
    assert_eq!(restored.size, Some(5));
    assert_eq!(read_all(&source, &item.id), b"first");
    assert_eq!(source.revisions(&item.id).unwrap().len(), 3);
}

#[test]
fn pinned_reads_come_from_the_revision_itself() {
    let (server, source) = setup();
    let item = upload(
        &source,
        UploadTarget::Create {
            parent: source.root(),
            name: "pin.txt".into(),
        },
        b"0123456789",
    )
    .unwrap();
    let mut download = source
        .read(&item.id, Some(2..5), item.revision.as_ref())
        .unwrap();
    assert_eq!(download.revision, item.revision);
    let mut got = Vec::new();
    download.body.read_to_end(&mut got).unwrap();
    assert_eq!(got, b"234");
    server.write_file(item.id.as_str(), b"changed");
    assert!(matches!(
        source.read(&item.id, Some(0..1), item.revision.as_ref()),
        Err(Error::Conflict { current: Some(_) })
    ));
}

#[test]
fn rate_limits_carry_backoff_hints() {
    let (server, source) = setup();
    // Drive sends no `Retry-After` with a 403 rate limit, and no delay is
    // invented for it: the backoff is the daemon's choice.
    server.inject(FakeFault::UserRateLimited);
    assert!(matches!(
        source.list(&source.root()),
        Err(Error::RateLimited { retry_after: None })
    ));
    server.inject(FakeFault::TooManyRequests);
    assert!(matches!(
        source.list(&source.root()),
        Err(Error::RateLimited { retry_after: Some(d) }) if d == Duration::from_secs(1)
    ));
    source.list(&source.root()).unwrap();
}

#[test]
fn folders_moving_across_the_root_boundary() {
    let server = FakeDrive::start();
    let account = drive(server.config(), REMOTE_TOKEN);
    let mounted = account.create_folder(&account.root(), "Mounted").unwrap();
    let outside = account.create_folder(&account.root(), "Outside").unwrap();
    let project = account.create_folder(&outside.id, "Project").unwrap();
    let deep = account.create_folder(&project.id, "Deep").unwrap();
    let file = server.put_file(deep.id.as_str(), "spec.md", b"s");
    let source = drive(
        server.config().with_root(mounted.id.0.clone()),
        SOURCE_TOKEN,
    );

    let (initial, cursor) = drain(&source, None);
    assert!(initial.is_empty(), "{initial:?}");
    // Outside changes are not upserted.
    server.write_file(&file, b"s2");
    let (delta, cursor) = drain(&source, Some(&cursor));
    assert!(
        !delta.iter().any(|c| matches!(c, Change::Upsert(_))),
        "{delta:?}"
    );

    // Moving a folder in brings its unchanged contents along.
    account
        .move_item(&project.id, &mounted.id, "Project", None)
        .unwrap();
    let (delta, cursor) = drain(&source, Some(&cursor));
    for id in [project.id.as_str(), deep.id.as_str(), file.as_str()] {
        assert!(
            delta
                .iter()
                .any(|c| matches!(c, Change::Upsert(item) if item.id.as_str() == id)),
            "{id} missing from {delta:?}"
        );
    }

    // Moving it out again removes it.
    account
        .move_item(&project.id, &outside.id, "Project", None)
        .unwrap();
    let (delta, _) = drain(&source, Some(&cursor));
    assert!(
        delta
            .iter()
            .any(|c| matches!(c, Change::Remove(id) if *id == project.id))
    );
}

#[test]
fn enumeration_lists_many_folders_per_request() {
    let (server, source) = setup();
    for i in 0..30 {
        let dir = source
            .create_folder(&source.root(), &format!("dir{i}"))
            .unwrap();
        server.put_file(dir.id.as_str(), "leaf.txt", b"l");
    }
    let before = server.stats().file_lists;
    let (changes, _) = drain(&source, None);
    let lists = server.stats().file_lists - before;
    assert_eq!(
        changes
            .iter()
            .filter(|c| matches!(c, Change::Upsert(i) if i.name == "leaf.txt"))
            .count(),
        30
    );
    // The root, then 30 folders in batches of 20.
    assert_eq!(lists, 3, "one query per batch of folders");
}

#[test]
fn wait_polls_until_something_changes() {
    let server = FakeDrive::start();
    let source = drive(
        DriveConfig {
            poll_interval: Duration::from_millis(20),
            ..server.config()
        },
        SOURCE_TOKEN,
    );
    let (_, cursor) = drain(&source, None);
    assert_eq!(
        source
            .wait(Some(&cursor), Duration::from_millis(100))
            .unwrap(),
        Wait::Idle
    );
    server.put_file("root", "new.txt", b"n");
    assert_eq!(
        source.wait(Some(&cursor), Duration::from_secs(5)).unwrap(),
        Wait::Changed
    );
    server.inject(FakeFault::TooManyRequests);
    assert_eq!(
        source.wait(Some(&cursor), Duration::from_secs(5)).unwrap(),
        Wait::Backoff(Duration::from_secs(1))
    );
}

#[test]
fn shared_drives_need_their_parameters() {
    let server = FakeDrive::start();
    let team = server.add_shared_drive("Team");
    let source = drive(
        DriveConfig::shared_drive(team.clone()).with_api(server.url()),
        SOURCE_TOKEN,
    );
    assert_eq!(source.root(), ItemId::new(team));
    let docs = source.create_folder(&source.root(), "Docs").unwrap();
    upload(
        &source,
        UploadTarget::Create {
            parent: docs.id.clone(),
            name: "a.txt".into(),
        },
        b"a",
    )
    .unwrap();
    let listed = source.list(&docs.id).unwrap();
    assert_eq!(listed.len(), 1);
    let (changes, _) = drain(&source, None);
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(matches!(source.quota(), Err(Error::Unsupported(_))));
    // My Drive does not see shared drive items in its listings.
    let my_drive = drive(server.config(), SOURCE_TOKEN);
    assert!(my_drive.list(&my_drive.root()).unwrap().is_empty());
}

#[test]
fn only_bearer_tokens_are_accepted() {
    let server = FakeDrive::start();
    let source = GDrive::new(
        server.config(),
        Arc::new(StaticCredential(Credential::Basic {
            username: "ada".into(),
            password: Zeroizing::new("pw".into()),
        })),
    )
    .unwrap();
    assert!(matches!(
        source.list(&source.root()),
        Err(Error::Unsupported(_))
    ));
}
