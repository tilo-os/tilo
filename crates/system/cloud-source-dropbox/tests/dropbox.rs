//! Dropbox-specific behaviour beyond the shared conformance suite, against
//! the in-process fake: long-poll backoff, content-hash verification,
//! upload-session recovery, deletions the source never saw, moves in the
//! change feed, share links, quota, team spaces and non-ASCII names.

#![allow(clippy::unwrap_used, clippy::panic, reason = "test bodies")]

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use cloud_source::tree::Tree;
use cloud_source::{
    Change, CloudSource, Credential, Cursor, Error, ItemId, StaticCredential, UploadTarget, Wait,
};
use cloud_source_dropbox::fake::{FakeDropbox, FakeFault, TEAM_NAMESPACE, TOKEN};
use cloud_source_dropbox::{DropboxConfig, DropboxSource};
use zeroize::Zeroizing;

fn source_for(config: DropboxConfig) -> DropboxSource {
    let credential = Credential::Bearer {
        token: Zeroizing::new(TOKEN.to_owned()),
        expires: None,
    };
    DropboxSource::new(config, Arc::new(StaticCredential(credential))).unwrap()
}

fn source(server: &FakeDropbox) -> DropboxSource {
    source_for(server.config(""))
}

fn read_all(source: &dyn CloudSource, id: &ItemId) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    source.read(id, None, None)?.body.read_to_end(&mut body)?;
    Ok(body)
}

fn find(source: &dyn CloudSource, folder: &ItemId, name: &str) -> cloud_source::Item {
    source
        .list(folder)
        .unwrap()
        .into_iter()
        .find(|item| item.name == name)
        .unwrap_or_else(|| panic!("{name} is not listed"))
}

/// Drain the feed from `cursor`, returning all changes and the final cursor.
fn drain(source: &dyn CloudSource, cursor: Option<&Cursor>) -> (Vec<Change>, Cursor, bool) {
    let mut page = source.changes(cursor).unwrap();
    let reset = page.reset;
    let mut changes = std::mem::take(&mut page.changes);
    while page.has_more {
        page = source.changes(Some(&page.cursor)).unwrap();
        changes.append(&mut page.changes);
    }
    (changes, page.cursor, reset)
}

fn apply(tree: &mut Tree, changes: Vec<Change>) {
    for change in changes {
        match change {
            Change::Upsert(item) => {
                tree.upsert(item);
            }
            Change::Remove(id) => {
                tree.remove(&id);
            }
        }
    }
}

fn names_under(tree: &Tree, folder: &ItemId) -> Vec<String> {
    let mut names: Vec<String> = tree
        .items()
        .filter(|item| item.parents.contains(folder))
        .map(|item| item.name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn longpoll_idles_and_honours_backoff_without_asking_again() {
    let server = FakeDropbox::start();
    server.set_longpoll_unit(Duration::from_millis(5));
    let source = source(&server);
    let (_, cursor, _) = drain(&source, None);
    assert_eq!(
        source.wait(Some(&cursor), Duration::from_secs(30)).unwrap(),
        Wait::Idle
    );
    server.inject(FakeFault::Backoff(60));
    let first = source.wait(Some(&cursor), Duration::from_secs(30)).unwrap();
    assert_eq!(first, Wait::Backoff(Duration::from_secs(60)));
    let polls = server.count("files/list_folder/longpoll");
    match source.wait(Some(&cursor), Duration::from_secs(30)).unwrap() {
        Wait::Backoff(remaining) => assert!(remaining <= Duration::from_secs(60)),
        other => panic!("a pending backoff must be honoured: {other:?}"),
    }
    assert_eq!(
        server.count("files/list_folder/longpoll"),
        polls,
        "no request while backing off"
    );
}

#[test]
fn longpoll_wakes_on_a_change_in_scope() {
    let server = FakeDropbox::start();
    let source = source(&server);
    let (_, cursor, _) = drain(&source, None);
    let waiter = std::thread::spawn(move || source.wait(Some(&cursor), Duration::from_secs(60)));
    // The waiter holds its request open until the change lands.
    server.put("/woken.txt", b"x");
    assert_eq!(waiter.join().unwrap().unwrap(), Wait::Changed);
}

#[test]
fn corrupted_downloads_fail_content_hash_verification() {
    let server = FakeDropbox::start();
    server.put("/photo.jpg", &[7_u8; 10_000]);
    let source = source(&server);
    let item = find(&source, &source.root(), "photo.jpg");
    assert_eq!(
        item.hash(&cloud_source::HashAlgorithm::DropboxContentHash),
        Some(cloud_source_dropbox::content_hash::content_hash(&[7_u8; 10_000]).as_str())
    );
    server.inject(FakeFault::CorruptBody);
    let error = read_all(&source, &item.id).unwrap_err();
    assert!(
        matches!(&error, Error::Io(io) if io.kind() == std::io::ErrorKind::InvalidData),
        "{error:?}"
    );
    assert_eq!(read_all(&source, &item.id).unwrap(), vec![7_u8; 10_000]);
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i.wrapping_mul(7) % 251).unwrap())
        .collect()
}

#[test]
fn a_lost_append_response_resumes_from_the_saved_state() {
    let server = FakeDropbox::start();
    let source = source(&server);
    let chunk = 64 * 1024;
    let bytes = pattern(chunk * 3 + 5);
    let len = bytes.len() as u64;
    let mut session = source
        .begin_upload(
            UploadTarget::Create {
                parent: source.root(),
                name: "big.bin".into(),
            },
            len,
        )
        .unwrap();
    session
        .write_chunk(
            Box::new(std::io::Cursor::new(bytes[..chunk].to_vec())),
            chunk as u64,
        )
        .unwrap();
    let state = session.state().unwrap();
    server.inject(FakeFault::DropResponse);
    let lost = session.write_chunk(
        Box::new(std::io::Cursor::new(bytes[chunk..2 * chunk].to_vec())),
        chunk as u64,
    );
    assert!(matches!(lost, Err(Error::Offline)), "{lost:?}");
    drop(session);

    // The server applied the chunk; the saved state does not know.
    let mut resumed = source.resume_upload(&state).unwrap();
    assert_eq!(resumed.offset(), chunk as u64);
    let owned = bytes.clone();
    cloud_source::drive(resumed.as_mut(), len, chunk as u64, &mut |offset| {
        let start = usize::try_from(offset).unwrap();
        Ok(Box::new(std::io::Cursor::new(owned[start..].to_vec())))
    })
    .unwrap();
    let item = resumed.finish().unwrap();
    assert_eq!(item.size, Some(len));
    assert_eq!(server.get("/big.bin").unwrap(), bytes);
    assert_eq!(server.open_sessions(), 0);
}

#[test]
fn a_session_resumed_on_a_block_boundary_still_sends_the_whole_hash() {
    let server = FakeDropbox::start();
    let mut config = server.config("");
    config.upload_chunk = 4 * 1024 * 1024;
    let source = source_for(config);
    let chunk = 4 * 1024 * 1024;
    let bytes = pattern(chunk + 1000);
    let len = bytes.len() as u64;
    let mut session = source
        .begin_upload(
            UploadTarget::Create {
                parent: source.root(),
                name: "blocks.bin".into(),
            },
            len,
        )
        .unwrap();
    session
        .write_chunk(
            Box::new(std::io::Cursor::new(bytes[..chunk].to_vec())),
            chunk as u64,
        )
        .unwrap();
    let state = session.state().unwrap();
    assert!(state.0.contains("\"blocks\":["), "{}", state.0);
    drop(session);
    let mut resumed = source.resume_upload(&state).unwrap();
    resumed
        .write_chunk(
            Box::new(std::io::Cursor::new(bytes[chunk..].to_vec())),
            1000,
        )
        .unwrap();
    // The fake rejects a finish whose content_hash is not the file's.
    resumed.finish().unwrap();
    assert_eq!(server.get("/blocks.bin").unwrap(), bytes);
}

#[test]
fn an_expired_session_must_start_again() {
    let server = FakeDropbox::start();
    let source = source(&server);
    let state = cloud_source::UploadState(
        serde_json::json!({
            "session": "pid_upload_session:gone", "offset": 65536, "size": 200_000,
            "commit": {"path": "/x.bin", "mode": "Add"}, "blocks": null
        })
        .to_string(),
    );
    let mut resumed = source.resume_upload(&state).unwrap();
    let result = resumed.write_chunk(Box::new(std::io::Cursor::new(vec![0; 10])), 10);
    assert!(matches!(result, Err(Error::SessionExpired)), "{result:?}");
}

#[test]
fn deletions_the_source_never_saw_are_attributed_or_restart_the_feed() {
    let server = FakeDropbox::start();
    server.put("/keep.txt", b"k");
    server.put("/file.txt", b"f");
    server.put("/Folder/inner.txt", b"i");
    let first = source(&server);
    let (initial, cursor, _) = drain(&first, None);
    let file_id = initial
        .iter()
        .find_map(|c| match c {
            Change::Upsert(item) if item.name == "file.txt" => Some(item.id.clone()),
            _ => None,
        })
        .unwrap();

    // A new process with the persisted cursor and an empty path map.
    server.remove("/file.txt");
    let restarted = source(&server);
    let (delta, cursor, reset) = drain(&restarted, Some(&cursor));
    assert!(!reset);
    assert!(
        delta
            .iter()
            .any(|c| matches!(c, Change::Remove(id) if *id == file_id)),
        "a deleted file is found through its revision history: {delta:?}"
    );

    server.remove("/Folder");
    let again = source(&server);
    let (all, _, reset) = drain(&again, Some(&cursor));
    assert!(
        reset,
        "an unattributable folder deletion restarts the enumeration"
    );
    let names: Vec<&str> = all
        .iter()
        .filter_map(|c| match c {
            Change::Upsert(item) => Some(item.name.as_str()),
            Change::Remove(_) => None,
        })
        .collect();
    assert_eq!(names, ["keep.txt"]);
}

#[test]
fn moves_and_folder_deletions_keep_the_projected_tree_consistent() {
    let server = FakeDropbox::start();
    server.put("/Docs/a.txt", b"a");
    server.put("/Docs/Sub/b.txt", b"b");
    let local = source(&server);
    let remote = source(&server);
    let mut tree = Tree::new(local.root());
    let (initial, cursor, _) = drain(&local, None);
    apply(&mut tree, initial);
    let docs = find(&local, &local.root(), "Docs");

    remote
        .move_item(&docs.id, &remote.root(), "Archive", None)
        .unwrap();
    let (delta, cursor, _) = drain(&local, Some(&cursor));
    apply(&mut tree, delta);
    let archive = tree.get(&docs.id).expect("the moved folder keeps its id");
    assert_eq!(archive.name, "Archive");
    assert_eq!(names_under(&tree, &docs.id), ["Sub", "a.txt"]);

    let sub = find(&local, &docs.id, "Sub");
    remote.delete(&docs.id, None).unwrap();
    let (delta, _, _) = drain(&local, Some(&cursor));
    apply(&mut tree, delta);
    assert!(tree.get(&docs.id).is_none());
    assert!(
        tree.get(&sub.id).is_none(),
        "contents of a deleted folder are removed too"
    );
    assert!(names_under(&tree, &local.root()).is_empty());
}

#[test]
fn names_are_case_insensitive_and_keep_their_case() {
    let server = FakeDropbox::start();
    server.put("/Readme.TXT", b"r");
    let source = source(&server);
    let result = cloud_source::upload(
        &source,
        UploadTarget::Create {
            parent: source.root(),
            name: "readme.txt".into(),
        },
        1,
        |_| Ok(Box::new(std::io::Cursor::new(b"x".to_vec()))),
    );
    assert!(matches!(result, Err(Error::AlreadyExists)), "{result:?}");
    assert_eq!(find(&source, &source.root(), "Readme.TXT").size, Some(1));
    let folder = source.create_folder(&source.root(), "Café ☕").unwrap();
    let item = cloud_source::upload(
        &source,
        UploadTarget::Create {
            parent: folder.id.clone(),
            name: "naïve €.txt".into(),
        },
        3,
        |_| Ok(Box::new(std::io::Cursor::new(b"abc".to_vec()))),
    )
    .unwrap();
    assert_eq!(item.name, "naïve €.txt");
    assert_eq!(item.parents, vec![folder.id]);
    assert_eq!(read_all(&source, &item.id).unwrap(), b"abc");
    assert_eq!(
        server.paths(),
        ["/Café ☕", "/Café ☕/naïve €.txt", "/Readme.TXT"]
    );
}

#[test]
fn share_links_are_created_once() {
    let server = FakeDropbox::start();
    server.put("/share.txt", b"s");
    let source = source(&server);
    let item = find(&source, &source.root(), "share.txt");
    let first = source.share_link(&item.id).unwrap();
    assert!(first.starts_with("https://www.dropbox.com/"));
    assert_eq!(source.share_link(&item.id).unwrap(), first);
}

#[test]
fn quota_reports_usage_and_allocation() {
    let server = FakeDropbox::start();
    server.set_quota(1_000_000);
    server.put("/ten.bin", &[0; 10]);
    let quota = source(&server).quota().unwrap();
    assert_eq!(quota.used, 10);
    assert_eq!(quota.total, Some(1_000_000));
}

#[test]
fn team_members_find_and_use_their_team_space() {
    let server = FakeDropbox::team();
    let plain = source(&server);
    assert_eq!(plain.team_root().unwrap().as_deref(), Some(TEAM_NAMESPACE));
    assert_eq!(source(&FakeDropbox::start()).team_root().unwrap(), None);
    let mut wrong = server.config("");
    wrong.path_root = Some("9999".into());
    let result = source_for(wrong).list(&ItemId::new(""));
    assert!(matches!(result, Err(Error::Status(422))), "{result:?}");
}

#[test]
fn rate_limits_carry_the_retry_after() {
    let server = FakeDropbox::start();
    let source = source(&server);
    server.inject(FakeFault::RateLimited);
    let result = source.list(&source.root());
    assert!(
        matches!(
            result,
            Err(Error::RateLimited {
                retry_after: Some(d)
            }) if d == Duration::from_secs(1)
        ),
        "{result:?}"
    );
}

#[test]
fn stale_preconditions_never_reach_the_server() {
    let server = FakeDropbox::start();
    server.put("/a.txt", b"a");
    let source = source(&server);
    let item = find(&source, &source.root(), "a.txt");
    let uploads = server.count("files/upload");
    let result = cloud_source::upload(
        &source,
        UploadTarget::Replace {
            id: item.id,
            expect: Some(cloud_source::Revision::new("\"etag-from-elsewhere\"")),
        },
        1,
        |_| Ok(Box::new(std::io::Cursor::new(b"b".to_vec()))),
    );
    assert!(matches!(result, Err(Error::Conflict { .. })), "{result:?}");
    assert_eq!(server.count("files/upload"), uploads);
    assert_eq!(server.get("/a.txt").unwrap(), b"a");
}
