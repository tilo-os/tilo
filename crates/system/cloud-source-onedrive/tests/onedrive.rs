//! `OneDrive`-specific behaviour beyond the shared conformance suite, on
//! the fake Graph drive.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "tests"
)]

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use cloud_source::{
    Change, CloudSource, Credential, Error, HashAlgorithm, ItemId, StaticCredential, UploadTarget,
};
use cloud_source_onedrive::fake::{FakeFault, FakeGraph, QUOTA_TOTAL, TOKEN};
use cloud_source_onedrive::quickxor::quick_xor_base64;
use cloud_source_onedrive::{FRAGMENT_UNIT, OneDriveSource, RootSelector};
use zeroize::Zeroizing;

fn credential() -> Arc<StaticCredential> {
    Arc::new(StaticCredential(Credential::Bearer {
        token: Zeroizing::new(TOKEN.to_owned()),
        expires: None,
    }))
}

fn connect(server: &FakeGraph, root: RootSelector) -> OneDriveSource {
    OneDriveSource::connect(&server.config(root), credential()).unwrap()
}

fn upload(
    source: &OneDriveSource,
    target: UploadTarget,
    bytes: &[u8],
) -> cloud_source::Result<cloud_source::Item> {
    let owned = bytes.to_vec();
    cloud_source::upload(source, target, bytes.len() as u64, move |offset| {
        Ok(Box::new(std::io::Cursor::new(
            owned[usize::try_from(offset).unwrap()..].to_vec(),
        )))
    })
}

fn read_all(source: &OneDriveSource, id: &ItemId) -> Vec<u8> {
    let mut out = Vec::new();
    source
        .read(id, None, None)
        .unwrap()
        .body
        .read_to_end(&mut out)
        .unwrap();
    out
}

fn drain(
    source: &OneDriveSource,
    cursor: Option<&cloud_source::Cursor>,
) -> (Vec<Change>, cloud_source::Cursor) {
    let mut page = source.changes(cursor).unwrap();
    let mut changes = std::mem::take(&mut page.changes);
    while page.has_more {
        page = source.changes(Some(&page.cursor)).unwrap();
        changes.append(&mut page.changes);
    }
    (changes, page.cursor)
}

#[test]
fn a_folder_mount_sees_only_its_own_subtree_in_the_delta_feed() {
    let server = FakeGraph::business();
    server.put("Other/secret.txt", b"not yours");
    let root = server.mkdirs("Team/Mounted");
    server.put("Team/Mounted/inside.txt", b"yours");
    let source = connect(&server, RootSelector::Item(root));
    let (initial, cursor) = drain(&source, None);
    let names: Vec<String> = initial
        .iter()
        .filter_map(|c| match c {
            Change::Upsert(item) => Some(item.name.clone()),
            Change::Remove(_) => None,
        })
        .collect();
    assert_eq!(names, ["inside.txt"], "ancestors and siblings stay outside");

    // A change elsewhere is not an upsert here; moving an item out of the
    // mount removes it.
    server.put("Other/secret.txt", b"changed elsewhere");
    let inside = source.item_at("inside.txt").unwrap();
    let other = server.mkdirs("Other");
    source
        .move_item(&inside.id, &ItemId::new(other), "inside.txt", None)
        .unwrap();
    let (delta, _) = drain(&source, Some(&cursor));
    assert!(
        !delta
            .iter()
            .any(|c| matches!(c, Change::Upsert(item) if item.name == "secret.txt")),
        "{delta:?}"
    );
    assert!(
        delta
            .iter()
            .any(|c| matches!(c, Change::Remove(id) if *id == inside.id))
    );
}

#[test]
fn names_are_case_insensitive_and_paths_resolve() {
    let server = FakeGraph::personal();
    server.mkdirs("Work");
    let source = connect(&server, RootSelector::Path("/Work".into()));
    let created = upload(
        &source,
        UploadTarget::Create {
            parent: source.root(),
            name: "Report.txt".into(),
        },
        b"one",
    )
    .unwrap();
    let clash = upload(
        &source,
        UploadTarget::Create {
            parent: source.root(),
            name: "REPORT.TXT".into(),
        },
        b"two",
    );
    assert!(matches!(clash, Err(Error::AlreadyExists)), "{clash:?}");
    assert!(matches!(
        source.create_folder(&source.root(), "report.txt"),
        Err(Error::AlreadyExists)
    ));
    assert_eq!(source.item_at("/report.TXT").unwrap().id, created.id);
    assert!(source.capabilities().case_insensitive);
    // Renaming only the case of an item is not a clash with itself.
    let renamed = source
        .move_item(
            &created.id,
            &source.root(),
            "REPORT.txt",
            created.revision.as_ref(),
        )
        .unwrap();
    assert_eq!(renamed.name, "REPORT.txt");
}

#[test]
fn hashes_are_quick_xor_and_sha256_on_personal_drives() {
    let server = FakeGraph::personal();
    server.put("a.bin", b"hello world");
    let source = connect(&server, RootSelector::DriveRoot);
    let item = source.item_at("a.bin").unwrap();
    assert_eq!(
        item.hash(&HashAlgorithm::QuickXor),
        Some(quick_xor_base64(b"hello world").as_str())
    );
    assert_eq!(
        item.hash(&HashAlgorithm::Sha256),
        Some("b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9")
    );
    let business = FakeGraph::business();
    business.put("a.bin", b"hello world");
    let item = connect(&business, RootSelector::DriveRoot)
        .item_at("a.bin")
        .unwrap();
    assert!(item.hash(&HashAlgorithm::QuickXor).is_some());
    assert!(item.hash(&HashAlgorithm::Sha256).is_none());
}

#[test]
fn uploads_use_320_kib_fragments_and_skip_what_the_server_already_has() {
    let server = FakeGraph::personal();
    let source = connect(&server, RootSelector::DriveRoot);
    assert_eq!(source.capabilities().upload_chunk % FRAGMENT_UNIT, 0);
    let len = FRAGMENT_UNIT * 3 + 1;
    let bytes: Vec<u8> = (0..len).map(|i| (i % 253) as u8).collect();
    let mut session = source
        .begin_upload(
            UploadTarget::Create {
                parent: source.root(),
                name: "big.bin".into(),
            },
            len,
        )
        .unwrap();
    let unit = usize::try_from(FRAGMENT_UNIT).unwrap();
    session
        .write_chunk(
            Box::new(std::io::Cursor::new(bytes[..unit].to_vec())),
            FRAGMENT_UNIT,
        )
        .unwrap();
    // Fragments must be 320 KiB multiples except the last.
    assert!(matches!(
        session.write_chunk(Box::new(std::io::Cursor::new(vec![0; 10])), 10),
        Err(Error::Unsupported(_))
    ));
    // The first fragment again (a retry after a lost response): the source
    // learns the server's offset instead of failing.
    let state = session.state().unwrap();
    drop(session);
    let mut resumed = source.resume_upload(&state).unwrap();
    assert_eq!(resumed.offset(), FRAGMENT_UNIT);
    let owned = bytes.clone();
    cloud_source::drive(resumed.as_mut(), len, FRAGMENT_UNIT, &mut |offset| {
        Ok(Box::new(std::io::Cursor::new(
            owned[usize::try_from(offset).unwrap()..].to_vec(),
        )))
    })
    .unwrap();
    let item = resumed.finish().unwrap();
    assert_eq!(item.size, Some(len));
    assert_eq!(read_all(&source, &item.id), bytes);
    assert_eq!(server.open_uploads(), 0);
    assert_eq!(server.stats().bearer_leaks, 0);

    // An aborted session is gone.
    let session = source
        .begin_upload(
            UploadTarget::Create {
                parent: source.root(),
                name: "abandoned.bin".into(),
            },
            len,
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
fn empty_files_upload_without_a_session() {
    let server = FakeGraph::personal();
    let source = connect(&server, RootSelector::DriveRoot);
    let item = upload(
        &source,
        UploadTarget::Create {
            parent: source.root(),
            name: "empty.txt".into(),
        },
        b"",
    )
    .unwrap();
    assert_eq!(item.size, Some(0));
    assert!(matches!(
        upload(
            &source,
            UploadTarget::Create {
                parent: source.root(),
                name: "EMPTY.txt".into(),
            },
            b"",
        ),
        Err(Error::AlreadyExists)
    ));
    let stale = upload(
        &source,
        UploadTarget::Replace {
            id: item.id,
            expect: Some(cloud_source::Revision::new("\"stale,1\"")),
        },
        b"",
    );
    assert!(matches!(stale, Err(Error::Conflict { .. })), "{stale:?}");
}

#[test]
fn deletes_go_to_the_recycle_bin() {
    let server = FakeGraph::personal();
    server.put("Old/draft.txt", b"d");
    let source = connect(&server, RootSelector::DriveRoot);
    let folder = source.item_at("Old").unwrap();
    source.delete(&folder.id, folder.revision.as_ref()).unwrap();
    assert!(source.capabilities().trash);
    let mut bin = server.recycle_bin();
    bin.sort();
    assert_eq!(bin, ["Old", "draft.txt"]);
}

#[test]
fn share_links_and_quota() {
    let server = FakeGraph::business();
    server.put("share.txt", b"0123456789");
    let source = connect(&server, RootSelector::DriveRoot);
    let item = source.item_at("share.txt").unwrap();
    let link = source.share_link(&item.id).unwrap();
    assert!(link.starts_with("https://"));
    assert_eq!(
        source.share_link(&item.id).unwrap(),
        link,
        "the existing link is reused"
    );
    let quota = source.quota().unwrap();
    assert_eq!(quota.total, Some(QUOTA_TOTAL));
    assert_eq!(quota.used, 10, "business drives derive used from remaining");
}

#[test]
fn throttling_carries_retry_after() {
    let server = FakeGraph::personal();
    let source = connect(&server, RootSelector::DriveRoot);
    server.inject(FakeFault::RateLimited);
    let error = source.list(&source.root()).unwrap_err();
    assert!(
        matches!(error, Error::RateLimited { retry_after: Some(d) } if d == Duration::from_secs(1)),
        "{error:?}"
    );
    source.list(&source.root()).unwrap();
}

#[test]
fn listings_and_delta_follow_pages() {
    let server = FakeGraph::personal();
    server.set_page_size(3);
    for i in 0..10 {
        server.put(&format!("Many/f{i:02}.txt"), b"x");
    }
    let source = connect(&server, RootSelector::Path("Many".into()));
    assert_eq!(source.list(&source.root()).unwrap().len(), 10);
    let (all, _) = drain(&source, None);
    assert_eq!(
        all.iter()
            .filter(|c| matches!(c, Change::Upsert(_)))
            .count(),
        10
    );
    assert!(server.stats().delta_requests >= 4);
}

#[test]
fn ranged_reads_fetch_only_the_range() {
    let server = FakeGraph::personal();
    let bytes: Vec<u8> = (0..10_000_u32).map(|i| (i % 256) as u8).collect();
    server.put("r.bin", &bytes);
    let source = connect(&server, RootSelector::DriveRoot);
    let item = source.item_at("r.bin").unwrap();
    let before = server.stats();
    let mut download = source.read(&item.id, Some(100..200), None).unwrap();
    let mut got = Vec::new();
    download.body.read_to_end(&mut got).unwrap();
    assert_eq!(got, &bytes[100..200]);
    assert_eq!(download.total, Some(10_000));
    let after = server.stats();
    assert_eq!(after.ranged_downloads, before.ranged_downloads + 1);
    assert_eq!(after.bytes_served - before.bytes_served, 100);
    // A range past the end is empty without a download.
    let empty = source.read(&item.id, Some(20_000..20_010), None).unwrap();
    assert_eq!(empty.range.start, empty.range.end);
}

#[test]
fn a_rejected_token_is_reported() {
    let server = FakeGraph::personal();
    let source = connect(&server, RootSelector::DriveRoot);
    let wrong = OneDriveSource::new(
        source.mount().clone(),
        Arc::new(StaticCredential(Credential::Bearer {
            token: Zeroizing::new("nope".into()),
            expires: None,
        })),
    )
    .unwrap();
    assert!(matches!(
        wrong.list(&wrong.root()),
        Err(Error::Unauthorized)
    ));
    let basic = OneDriveSource::new(
        source.mount().clone(),
        Arc::new(StaticCredential(Credential::Basic {
            username: "ada".into(),
            password: Zeroizing::new("pw".into()),
        })),
    )
    .unwrap();
    assert!(matches!(
        basic.list(&basic.root()),
        Err(Error::Unsupported(_))
    ));
}
