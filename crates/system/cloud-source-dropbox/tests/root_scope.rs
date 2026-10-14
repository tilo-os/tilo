//! Mount boundaries apply to id-addressed operations, not just path listings.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
use cloud_source::{CloudSource, Credential, Error, StaticCredential, UploadTarget};
use cloud_source_dropbox::{
    DropboxSource,
    fake::{FakeDropbox, TOKEN},
};
use std::{io::Read, sync::Arc};
use zeroize::Zeroizing;

#[test]
fn foreign_ids_cannot_list_read_replace_or_share_outside_a_mount() {
    let server = FakeDropbox::start();
    server.put("/Inside/keep.txt", b"keep");
    server.put("/Outside/private.txt", b"private");
    let source = |root| {
        DropboxSource::new(
            server.config(root),
            Arc::new(StaticCredential(Credential::Bearer {
                token: Zeroizing::new(TOKEN.into()),
                expires: None,
            })),
        )
        .unwrap()
    };
    let account = source("");
    let mounted = source("/Inside");
    let outside = account
        .list(&account.root())
        .unwrap()
        .into_iter()
        .find(|item| item.name == "Outside")
        .unwrap();
    let file = account.list(&outside.id).unwrap().pop().unwrap();
    assert!(matches!(mounted.list(&outside.id), Err(Error::NotFound)));
    assert!(matches!(
        mounted.read(&file.id, None, None),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        mounted.read(&file.id, Some(0..2), None),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        mounted.read_revision(&file.id, file.revision.as_ref().unwrap(), None),
        Err(Error::NotFound)
    ));
    for expect in [None, file.revision.clone()] {
        assert!(matches!(
            mounted.begin_upload(
                UploadTarget::Replace {
                    id: file.id.clone(),
                    expect
                },
                1
            ),
            Err(Error::NotFound)
        ));
    }
    assert!(matches!(mounted.share_link(&file.id), Err(Error::NotFound)));
    assert!(matches!(mounted.revisions(&file.id), Err(Error::NotFound)));
    assert_eq!(server.get("/Outside/private.txt").unwrap(), b"private");
    let inside = mounted.list(&mounted.root()).unwrap().pop().unwrap();
    let mut body = Vec::new();
    mounted
        .read(&inside.id, None, None)
        .unwrap()
        .body
        .read_to_end(&mut body)
        .unwrap();
    assert_eq!(body, b"keep");
    let mut session = mounted
        .begin_upload(
            UploadTarget::Replace {
                id: inside.id,
                expect: inside.revision,
            },
            1,
        )
        .unwrap();
    session
        .write_chunk(Box::new(std::io::Cursor::new(b"x".to_vec())), 1)
        .unwrap();
    session.finish().unwrap();
    assert_eq!(server.get("/Inside/keep.txt").unwrap(), b"x");
}

#[test]
fn resumed_and_moved_upload_targets_stay_within_the_mount() {
    let server = FakeDropbox::start();
    server.put("/Inside/keep.txt", b"keep");
    server.put("/Outside/private.txt", b"private");
    let source = |root| {
        DropboxSource::new(
            server.config(root),
            Arc::new(StaticCredential(Credential::Bearer {
                token: Zeroizing::new(TOKEN.into()),
                expires: None,
            })),
        )
        .unwrap()
    };
    let account = source("");
    let mounted = source("/Inside");
    let outside = account
        .list(&account.root())
        .unwrap()
        .into_iter()
        .find(|item| item.name == "Outside")
        .unwrap();
    let foreign = account.list(&outside.id).unwrap().pop().unwrap();
    let targets = [
        UploadTarget::Replace {
            id: foreign.id.clone(),
            expect: None,
        },
        UploadTarget::Replace {
            id: foreign.id,
            expect: foreign.revision,
        },
        UploadTarget::Create {
            parent: outside.id.clone(),
            name: "new.txt".into(),
        },
    ];
    for target in targets {
        let mut session = account.begin_upload(target, 2).unwrap();
        session
            .write_chunk(Box::new(std::io::Cursor::new(b"x".to_vec())), 1)
            .unwrap();
        let saved = session.state().unwrap();
        assert!(matches!(
            mounted.resume_upload(&saved),
            Err(Error::NotFound)
        ));
        session.abort().unwrap();
    }
    let inside = mounted.list(&mounted.root()).unwrap().pop().unwrap();
    let mut session = mounted
        .begin_upload(
            UploadTarget::Replace {
                id: inside.id.clone(),
                expect: None,
            },
            2,
        )
        .unwrap();
    session
        .write_chunk(Box::new(std::io::Cursor::new(b"xy".to_vec())), 2)
        .unwrap();
    // A one-chunk upload already commits. For a session-based upload, move
    // its target outside the mount after resume but before the commit.
    session.finish().unwrap();
    let mut session = mounted
        .begin_upload(
            UploadTarget::Replace {
                id: inside.id.clone(),
                expect: None,
            },
            2,
        )
        .unwrap();
    session
        .write_chunk(Box::new(std::io::Cursor::new(b"x".to_vec())), 1)
        .unwrap();
    let saved = session.state().unwrap();
    drop(session);
    let mut resumed = mounted.resume_upload(&saved).unwrap();
    resumed
        .write_chunk(Box::new(std::io::Cursor::new(b"z".to_vec())), 1)
        .unwrap();
    account
        .move_item(&inside.id, &outside.id, "moved.txt", None)
        .unwrap();
    assert!(matches!(resumed.finish(), Err(Error::NotFound)));
    assert_eq!(server.get("/Outside/private.txt").unwrap(), b"private");
    assert!(server.get("/Outside/new.txt").is_none());
    assert_eq!(server.get("/Outside/moved.txt").unwrap(), b"xy");
}
