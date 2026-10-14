//! Production-adapter checks against an explicitly supplied, disposable Rystic twin.
//! See docs/testing/rystic-dropbox-feedback.md for startup and limitations.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration test assertions"
)]

use cloud_source::{
    Change, CloudSource, Credential, Error, HashAlgorithm, Item, ItemId, StaticCredential,
    UploadTarget,
};
use cloud_source_dropbox::{DropboxConfig, DropboxSource, Endpoints, content_hash::content_hash};
use std::{
    io::{Cursor as Bytes, Read},
    sync::Arc,
};
use zeroize::Zeroizing;

fn source(root: &str) -> DropboxSource {
    let endpoint = std::env::var("TILO_RYSTIC_DROPBOX_URL")
        .expect("set TILO_RYSTIC_DROPBOX_URL to a disposable dropbox-twin origin");
    let url = reqwest::Url::parse(&endpoint).expect("valid twin URL");
    assert_eq!(url.scheme(), "http", "only local HTTP twins are allowed");
    assert!(
        matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
        "only loopback twins are allowed"
    );
    let http = tilo_net::http::blocking_client().build().unwrap();
    let version: serde_json::Value = http
        .get(format!(
            "{}/_rystic/version",
            endpoint.trim_end_matches('/')
        ))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(version["product"], "dropbox-twin");
    let mut config = DropboxConfig::new(root);
    let endpoint = endpoint.trim_end_matches('/').to_owned();
    config.endpoints = Endpoints {
        api: endpoint.clone(),
        content: endpoint.clone(),
        notify: endpoint,
    };
    config.upload_chunk = 32 * 1024;
    config.page_limit = Some(2);
    DropboxSource::new(
        config,
        Arc::new(StaticCredential(Credential::Bearer {
            token: Zeroizing::new("sl.tilo-test".into()),
            expires: None,
        })),
    )
    .unwrap()
}

fn upload(
    source: &DropboxSource,
    target: UploadTarget,
    bytes: &[u8],
) -> cloud_source::Result<Item> {
    cloud_source::upload(source, target, bytes.len() as u64, |_| {
        Ok(Box::new(Bytes::new(bytes.to_vec())))
    })
}

fn read(source: &DropboxSource, id: &ItemId) -> Vec<u8> {
    let mut body = Vec::new();
    source
        .read(id, None, None)
        .unwrap()
        .body
        .read_to_end(&mut body)
        .unwrap();
    body
}

fn import(source: &DropboxSource) -> Vec<Item> {
    let mut page = source.changes(None).unwrap();
    assert!(page.reset);
    let mut imported = Vec::new();
    loop {
        imported.extend(page.changes.into_iter().map(|change| match change {
            Change::Upsert(item) => item,
            Change::Remove(_) => panic!("fresh import contains a deletion"),
        }));
        if !page.has_more {
            break;
        }
        page = source.changes(Some(&page.cursor)).unwrap();
        assert!(!page.reset);
    }
    imported
}

#[test]
fn importing_writing_and_resuming_use_the_production_adapter() {
    let account = source("");
    let name = format!("tilo-rystic-{}", uuid::Uuid::new_v4());
    let folder = account.create_folder(&account.root(), &name).unwrap();
    let mounted = source(&format!("/{name}"));
    let nested = mounted.create_folder(&mounted.root(), "Café ☕").unwrap();
    let bytes: Vec<u8> = (0..100_003)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let mut session = mounted
        .begin_upload(
            UploadTarget::Create {
                parent: nested.id.clone(),
                name: "naïve 😀.bin".into(),
            },
            bytes.len() as u64,
        )
        .unwrap();
    session
        .write_chunk(Box::new(Bytes::new(bytes[..32768].to_vec())), 32768)
        .unwrap();
    let saved = session.state().unwrap();
    drop(session);
    let restarted = source(&format!("/{name}"));
    let mut session = restarted.resume_upload(&saved).unwrap();
    cloud_source::drive(session.as_mut(), bytes.len() as u64, 32768, &mut |offset| {
        Ok(Box::new(Bytes::new(
            bytes[usize::try_from(offset).unwrap()..].to_vec(),
        )))
    })
    .unwrap();
    let file = session.finish().unwrap();
    assert_eq!(file.parents.as_slice(), std::slice::from_ref(&nested.id));
    assert_eq!(
        file.hash(&HashAlgorithm::DropboxContentHash),
        Some(content_hash(&bytes).as_str())
    );
    assert_eq!(read(&mounted, &file.id), bytes);
    let mut range = mounted
        .read(&file.id, Some(17..91), file.revision.as_ref())
        .unwrap();
    let mut part = Vec::new();
    range.body.read_to_end(&mut part).unwrap();
    assert_eq!(range.range, 17..91);
    assert_eq!(part, bytes[17..91]);
    for name in ["a.txt", "b.txt", "c.txt"] {
        upload(
            &mounted,
            UploadTarget::Create {
                parent: mounted.root(),
                name: name.into(),
            },
            name.as_bytes(),
        )
        .unwrap();
    }
    assert_eq!(mounted.list(&mounted.root()).unwrap().len(), 4);
    let imported = import(&mounted);
    assert_eq!(imported.len(), 5);
    assert!(
        imported
            .iter()
            .any(|item| item.id == file.id && item.parents == [nested.id.clone()])
    );
    let first_rev = file.revision.clone().unwrap();
    assert!(
        mounted
            .revisions(&file.id)
            .unwrap()
            .iter()
            .any(|r| r.revision == first_rev)
    );
    let mut old = mounted.read_revision(&file.id, &first_rev, None).unwrap();
    let mut original = Vec::new();
    old.body.read_to_end(&mut original).unwrap();
    assert_eq!(original, bytes);
    let restored = mounted.restore_revision(&file.id, &first_rev).unwrap();
    assert_eq!(read(&mounted, &restored.id), bytes);
    let link = mounted.share_link(&file.id).unwrap();
    assert_eq!(mounted.share_link(&file.id).unwrap(), link);
    assert!(mounted.quota().unwrap().total.is_some());
    let moved = mounted
        .move_item(
            &file.id,
            &mounted.root(),
            "moved.bin",
            restored.revision.as_ref(),
        )
        .unwrap();
    assert_eq!(moved.id, file.id);
    assert_eq!(moved.parents, [mounted.root()]);
    mounted.delete(&moved.id, moved.revision.as_ref()).unwrap();
    assert!(matches!(mounted.stat(&moved.id), Err(Error::NotFound)));
    account.delete(&folder.id, None).unwrap();
}

// A strict vendor contract, opt-in separately because the released twin
// stores an id-addressed upload as a new path instead of replacing the file.
#[cfg(feature = "rystic-contract-tests")]
#[test]
fn id_addressed_replacement_preserves_identity_and_checks_revision() {
    let account = source("");
    let folder = account
        .create_folder(
            &account.root(),
            &format!("tilo-replace-{}", uuid::Uuid::new_v4()),
        )
        .unwrap();
    let original = upload(
        &account,
        UploadTarget::Create {
            parent: folder.id.clone(),
            name: "file.txt".into(),
        },
        b"original",
    )
    .unwrap();
    let changed = upload(
        &account,
        UploadTarget::Replace {
            id: original.id.clone(),
            expect: original.revision.clone(),
        },
        b"changed",
    )
    .unwrap();
    assert_eq!(changed.id, original.id);
    assert_eq!(read(&account, &original.id), b"changed");
    assert!(matches!(
        upload(
            &account,
            UploadTarget::Replace {
                id: original.id.clone(),
                expect: original.revision
            },
            b"stale"
        ),
        Err(Error::Conflict { .. })
    ));
    account.delete(&folder.id, None).unwrap();
}

#[cfg(feature = "rystic-contract-tests")]
#[test]
fn sync_delivers_new_items_after_initial_enumeration() {
    let account = source("");
    let name = format!("tilo-delta-{}", uuid::Uuid::new_v4());
    let folder = account.create_folder(&account.root(), &name).unwrap();
    let mounted = source(&format!("/{name}"));
    upload(
        &mounted,
        UploadTarget::Create {
            parent: mounted.root(),
            name: "z.txt".into(),
        },
        b"old",
    )
    .unwrap();
    let mut page = mounted.changes(None).unwrap();
    while page.has_more {
        page = mounted.changes(Some(&page.cursor)).unwrap();
    }
    let added = upload(
        &mounted,
        UploadTarget::Create {
            parent: mounted.root(),
            name: "a.txt".into(),
        },
        b"new",
    )
    .unwrap();
    let mut found = false;
    loop {
        page = mounted.changes(Some(&page.cursor)).unwrap();
        found |= page
            .changes
            .iter()
            .any(|change| matches!(change, Change::Upsert(item) if item.id == added.id));
        if !page.has_more {
            break;
        }
    }
    account.delete(&folder.id, None).unwrap();
    assert!(
        found,
        "sync must deliver items inserted before the previous sort position"
    );
}

#[test]
fn mounted_folder_rejects_foreign_ids() {
    let account = source("");
    let name = format!("tilo-scope-{}", uuid::Uuid::new_v4());
    let inside = account.create_folder(&account.root(), &name).unwrap();
    let outside = account
        .create_folder(&account.root(), &format!("{name}-outside"))
        .unwrap();
    let file = upload(
        &account,
        UploadTarget::Create {
            parent: outside.id.clone(),
            name: "private.txt".into(),
        },
        b"private",
    )
    .unwrap();
    let mounted = source(&format!("/{name}"));
    assert!(matches!(mounted.stat(&file.id), Err(Error::NotFound)));
    assert!(matches!(mounted.list(&outside.id), Err(Error::NotFound)));
    assert!(matches!(
        mounted.read(&file.id, None, None),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        mounted.begin_upload(
            UploadTarget::Replace {
                id: file.id.clone(),
                expect: file.revision.clone()
            },
            1
        ),
        Err(Error::NotFound)
    ));
    assert!(matches!(mounted.share_link(&file.id), Err(Error::NotFound)));
    assert_eq!(read(&account, &file.id), b"private");
    account.delete(&inside.id, None).unwrap();
    account.delete(&outside.id, None).unwrap();
}
