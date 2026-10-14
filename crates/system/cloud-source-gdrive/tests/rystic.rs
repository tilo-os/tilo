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

const TOKEN: &str = "tilo-rystic-gdrive";

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
    // listing, metadata updates and deletes below. Resumable uploads are not
    // served by this twin and must not be replaced with a simulator fallback.
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
    // renderings. Sync/upload contracts are separate tests, never assertions
    // that a missing vendor endpoint is correct behavior.
}

#[cfg(feature = "rystic-contract-tests")]
#[test]
fn initial_sync_obtains_a_change_cursor() {
    let (fixture, _) = fixture();
    let mut page = fixture.source.changes(None).unwrap();
    assert!(page.reset);
    while page.has_more {
        page = fixture.source.changes(Some(&page.cursor)).unwrap();
    }
    assert!(!page.cursor.0.is_empty());
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
