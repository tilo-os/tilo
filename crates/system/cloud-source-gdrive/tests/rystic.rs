//! Opt-in production-adapter checks against a dedicated Google Drive twin.
//! See docs/testing/rystic-gdrive-feedback.md for startup and invocation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions must fail when the twin cannot be exercised"
)]

use std::io::Read;
use std::sync::Arc;

use cloud_source::{CloudSource, Credential, Error, ItemId, ItemKind, StaticCredential};
use cloud_source_gdrive::{DriveConfig, GDrive, Url};
use serde_json::json;
use zeroize::Zeroizing;

// The twin validates Google's token prefix; this is deliberately not a credential.
const TOKEN: &str = "ya29.tilo-rystic-gdrive";

struct Fixture {
    source: GDrive,
    folder: ItemId,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // The caller owns a dedicated twin, but also remove our fixture tree
        // on assertion failures rather than leaving files for the next run.
        let _ = self.source.delete(&self.folder, None);
    }
}

fn bytes(source: &GDrive, id: &ItemId, range: Option<std::ops::Range<u64>>) -> Vec<u8> {
    let mut download = source.read(id, range, None).unwrap();
    let mut bytes = Vec::new();
    download.body.read_to_end(&mut bytes).unwrap();
    bytes
}

fn fixture() -> (Fixture, ItemId) {
    let endpoint = std::env::var("TILO_RYSTIC_GDRIVE_URL")
        .expect("set TILO_RYSTIC_GDRIVE_URL to a dedicated running google-drive-twin");
    let url = Url::parse(&endpoint).unwrap();
    assert_eq!(url.scheme(), "http", "only a local simulator is allowed");
    assert!(matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]")
    ));
    let http = tilo_net::http::blocking_client()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();
    let version: serde_json::Value = http
        .get(format!("{endpoint}/_rystic/version"))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(version.get("product").unwrap(), "google-drive-twin");
    eprintln!(
        "Google Drive twin {} model {}",
        version.get("version").unwrap(),
        version.get("model_hash").unwrap()
    );
    let source = GDrive::new(
        DriveConfig {
            trash: false,
            ..DriveConfig::my_drive().with_api(&endpoint)
        },
        Arc::new(StaticCredential(Credential::Bearer {
            token: Zeroizing::new(TOKEN.to_owned()),
            expires: None,
        })),
    )
    .unwrap();
    let folder = source
        .create_folder(
            &source.root(),
            &format!("tilo-import-{}", uuid::Uuid::new_v4()),
        )
        .unwrap();
    assert_eq!(folder.parents, vec![source.root()]);
    let fixture = Fixture {
        source,
        folder: folder.id,
    };

    // Fixture construction uses the twin's seed surface, not a second
    // provider implementation. The production adapter performs all reads,
    // listing, metadata updates and deletes below. Mutations under proof must
    // use the provider API, never seed-state edits or an HTTP fallback.
    let prefix = uuid::Uuid::new_v4().to_string();
    let mut files = serde_json::Map::new();
    let mut revisions = serde_json::Map::new();
    for n in 0..1001 {
        let id = format!("{prefix}-{n}");
        files.insert(
            id.clone(),
            json!({
                "id": id, "name": "same.txt", "mimeType": "text/plain",
                "parents": [fixture.folder.as_str()], "size": "10", "version": "1",
                "headRevisionId": "r1", "md5Checksum": "781e5e245d69b566979b86e28d23f2c7",
                "owners": [{"permissionId": "member"}]
            }),
        );
        revisions.insert(
            format!("{id}/r1"),
            json!({
                "fileId": id, "id": "r1", "mimeType": "text/plain", "size": "10",
                "body_b64": "MDEyMzQ1Njc4OQ=="
            }),
        );
    }
    http.post(format!("{endpoint}/_rystic/seed"))
        .json(&json!({
            "state_file": files, "state_revision": revisions,
            "state_about": {"drive#about": {
                "kind": "drive#about",
                "storageQuota": {"limit": "150000", "usage": "10010"}
            }}
        }))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    (fixture, ItemId::new(format!("{prefix}-0")))
}

#[test]
fn imports_and_mutations_through_the_production_adapter() {
    let (fixture, id) = fixture();
    let source = &fixture.source;
    let listed = source.list(&fixture.folder).unwrap();
    assert_eq!(
        listed.len(),
        1001,
        "all listing pages, including duplicate names"
    );
    let item = source.stat(&id).unwrap();
    assert_eq!(item.size, Some(10));
    assert_eq!(item.parents, vec![fixture.folder.clone()]);
    assert_eq!(item.kind, ItemKind::File);
    assert_eq!(bytes(source, &id, None), b"0123456789");
    assert_eq!(bytes(source, &id, Some(2..5)), b"234");
    assert_eq!(bytes(source, &id, Some(8..20)), b"89");
    let mut pinned = source
        .read(&id, Some(2..5), item.revision.as_ref())
        .unwrap();
    assert_eq!(pinned.revision, item.revision);
    let mut body = Vec::new();
    pinned.body.read_to_end(&mut body).unwrap();
    assert_eq!(body, b"234");
    assert_eq!(source.revisions(&id).unwrap().len(), 1);

    let nested = source.create_folder(&fixture.folder, "Nested").unwrap();
    let moved = source
        .move_item(&id, &nested.id, "renamed.txt", item.revision.as_ref())
        .unwrap();
    assert_eq!(moved.name, "renamed.txt");
    assert_eq!(moved.parents, vec![nested.id.clone()]);
    assert_eq!(source.list(&nested.id).unwrap(), vec![moved]);
    source.delete(&id, None).unwrap();
    assert!(matches!(source.stat(&id), Err(Error::NotFound)));

    let native = source
        .create_native_document(
            &fixture.folder,
            "Notes",
            "application/vnd.google-apps.document",
        )
        .unwrap();
    let ItemKind::Native(document) = native.kind else {
        panic!("not a native document")
    };
    assert_eq!(document.link_extension, "gdoc");
    assert!(matches!(
        source.read(&native.id, None, None),
        Err(Error::Unsupported(_))
    ));
    let quota = source.quota().unwrap();
    assert_eq!(quota.total, Some(150_000));
    assert_eq!(quota.used, 10_010);

    // No export-validity claim: the twin serves placeholders, not Office/PDF
    // renderings. Strict export/sync/upload contracts are separate tests, never assertions
    // that a missing vendor endpoint is correct behavior.
}

#[cfg(feature = "rystic-contract-tests")]
#[test]
fn native_document_exports_a_genuine_docx_package() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let (fixture, _) = fixture();
    let item = fixture
        .source
        .create_native_document(
            &fixture.folder,
            "Office export contract",
            "application/vnd.google-apps.document",
        )
        .unwrap();
    let ItemKind::Native(native) = &item.kind else {
        panic!("created entry is not a native Google document")
    };
    let docx = native
        .exports
        .iter()
        .find(|format| {
            format.extension == "docx"
                && format.mime
                    == "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        })
        .expect("native Google document must advertise the DOCX export format");
    let download = fixture.source.export(&item.id, docx).unwrap();
    let mut body = Vec::new();
    download
        .body
        .take(10 * 1024 * 1024 + 1)
        .read_to_end(&mut body)
        .unwrap();
    assert!(body.len() <= 10 * 1024 * 1024, "DOCX export too large");

    // Parse the ZIP directory, CRCs and required Office XML parts, not merely
    // a non-JSON response or a PK signature. Python avoids adding a production
    // dependency for an explicitly opted-in external simulator contract.
    let mut validator = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/validate_docx.py"
        ))
        .stdin(Stdio::piped())
        .spawn()
        .expect("the strict Office export contract requires Python 3");
    validator.stdin.take().unwrap().write_all(&body).unwrap();
    assert!(
        validator.wait().unwrap().success(),
        "native DOCX export must be a genuine ZIP Office Open XML package; see validator diagnostic"
    );
}

#[cfg(feature = "rystic-contract-tests")]
#[test]
fn initial_sync_obtains_a_change_cursor() {
    let (fixture, _) = fixture();
    let mut page = fixture.source.changes(None).unwrap();
    assert!(page.reset);
    let mut snapshot = std::collections::HashSet::new();
    for _ in 0..100 {
        for change in &page.changes {
            if let cloud_source::Change::Upsert(item) = change {
                assert!(snapshot.insert(item.id.clone()), "duplicate snapshot item");
            }
        }
        if !page.has_more {
            break;
        }
        page = fixture.source.changes(Some(&page.cursor)).unwrap();
        assert!(!page.reset);
    }
    assert!(!page.has_more, "initial pagination must terminate");
    assert_eq!(snapshot.len(), 1002, "folder and every seeded file");
    assert!(!page.cursor.0.is_empty());
    let idle = fixture.source.changes(Some(&page.cursor)).unwrap();
    assert!(!idle.reset && !idle.has_more && idle.changes.is_empty());
    assert_eq!(
        idle.cursor, page.cursor,
        "idle terminal cursor stays stable"
    );
}

#[cfg(feature = "rystic-contract-tests")]
#[test]
fn changes_after_cursor_paginate_and_resume_without_loss() {
    use cloud_source::{Change, Cursor};
    use std::collections::HashMap;

    let (fixture, _) = fixture();
    let source = &fixture.source;
    let destination = source
        .create_folder(&fixture.folder, "destination")
        .unwrap();
    let mut page = source.changes(None).unwrap();
    for _ in 0..100 {
        if !page.has_more {
            break;
        }
        page = source.changes(Some(&page.cursor)).unwrap();
    }
    assert!(!page.has_more, "initial snapshot must terminate");
    let saved = page.cursor.0;
    let items: Vec<_> = source
        .list(&fixture.folder)
        .unwrap()
        .into_iter()
        .filter(|item| item.id != destination.id)
        .collect();
    let mut expected = HashMap::new();
    // More than Drive's 1000-change page size, with exactly one mutation per
    // id so duplicate events cannot hide a missing item in a set comparison.
    for (n, item) in items.iter().enumerate() {
        if n == 0 {
            source.delete(&item.id, None).unwrap();
            expected.insert(item.id.clone(), None);
        } else {
            let parent = if n == 1 {
                &destination.id
            } else {
                &fixture.folder
            };
            let moved = source
                .move_item(&item.id, parent, &format!("changed-{n}"), None)
                .unwrap();
            expected.insert(item.id.clone(), Some(moved));
        }
    }
    // A fresh adapter has no in-memory folder cache or cursor state.
    let restarted = GDrive::new(
        DriveConfig {
            trash: false,
            ..DriveConfig::my_drive().with_api(std::env::var("TILO_RYSTIC_GDRIVE_URL").unwrap())
        },
        Arc::new(StaticCredential(Credential::Bearer {
            token: Zeroizing::new(TOKEN.to_owned()),
            expires: None,
        })),
    )
    .unwrap();
    let mut cursor = Cursor(saved);
    let mut observed = HashMap::new();
    let mut pages = 0;
    loop {
        let delta = restarted.changes(Some(&cursor)).unwrap();
        assert!(!delta.reset, "resume must not enumerate a full snapshot");
        for change in delta.changes {
            let (id, value) = match change {
                Change::Upsert(item) => (item.id.clone(), Some(item)),
                Change::Remove(id) => (id, None),
            };
            assert!(observed.insert(id, value).is_none(), "duplicate delta id");
        }
        cursor = delta.cursor;
        pages += 1;
        assert!(pages < 100, "delta pagination must terminate");
        if !delta.has_more {
            break;
        }
    }
    assert!(pages >= 2, "must exercise changes pagination");
    assert_eq!(observed, expected, "exact metadata and tombstones, no loss");
    let idle = restarted.changes(Some(&cursor)).unwrap();
    assert!(!idle.reset && !idle.has_more && idle.changes.is_empty());
    assert_eq!(idle.cursor, cursor, "stable terminal cursor");
}

#[cfg(feature = "rystic-contract-tests")]
#[test]
fn resumable_upload_commits_exact_bytes() {
    let (fixture, _) = fixture();
    let item = cloud_source::upload(
        &fixture.source,
        cloud_source::UploadTarget::Create {
            parent: fixture.folder.clone(),
            name: "uploaded.txt".into(),
        },
        10,
        |_| Ok(Box::new(std::io::Cursor::new(b"0123456789".to_vec()))),
    )
    .unwrap();
    assert_eq!(bytes(&fixture.source, &item.id, None), b"0123456789");
}
