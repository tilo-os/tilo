//! The cloud-source conformance suite for iCloud Drive:
//!
//! - against the in-process fake (always), with a trusted session it issues
//!   (custody's tests run the sign-in itself against the same fake);
//! - against a live account when `TILO_LIVE_ICLOUD_SESSION` holds a
//!   serialized [`StoredSession`] (JSON or base64 JSON, including its trust
//!   token), each run in its own `conformance-<uuid>` folder. Skipped when
//!   the variable is absent.
//!
//! A live session Apple no longer accepts is not a provider failure (custody
//! renews sessions; this suite only consumes one): the suite's checks skip, and
//! `live::session_is_current` fails with [`REAUTH_NEEDED`] in its message,
//! so a scheduled job tells "re-auth needed" apart from breakage (any other
//! failure).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures"
)]

use std::io::Read;
use std::sync::Arc;

use cloud_source::Error;
use cloud_source::conformance::{Fault, Fixture, SwitchableCredential};
use cloud_source::{CloudSource, ItemId, StaticCredential, UploadTarget};
use cloud_source_icloud::auth::StoredSession;
use cloud_source_icloud::fake::{FakeFault, FakeICloud};
use cloud_source_icloud::{ICloudConfig, ICloudDrive};

/// The marker a scheduled job greps for: the live session expired and a
/// person has to sign in again.
const REAUTH_NEEDED: &str = "ICLOUD_REAUTH_NEEDED";
const LIVE_SESSION_VAR: &str = "TILO_LIVE_ICLOUD_SESSION";

struct FakeFixture {
    server: FakeICloud,
    credentials: Arc<SwitchableCredential>,
    source: ICloudDrive,
    remote: ICloudDrive,
}

impl FakeFixture {
    fn start() -> Self {
        let server = FakeICloud::start();
        let session = server.session();
        let credentials = Arc::new(SwitchableCredential::new(Box::new(StaticCredential(
            session.credential(),
        ))));
        let source = ICloudDrive::new(server.config(), Arc::clone(&credentials) as _).unwrap();
        source
            .stat(&source.root())
            .expect("the signed-in session works");
        let remote = ICloudDrive::new(
            server.config(),
            Arc::new(StaticCredential(session.credential())),
        )
        .unwrap();
        Self {
            server,
            credentials,
            source,
            remote,
        }
    }
}

impl Fixture for FakeFixture {
    fn source(&self) -> &dyn CloudSource {
        &self.source
    }

    fn remote(&self) -> &dyn CloudSource {
        &self.remote
    }

    fn credentials(&self) -> &SwitchableCredential {
        &self.credentials
    }

    fn set_offline(&self, offline: bool) -> bool {
        self.server.set_offline(offline);
        true
    }

    fn inject(&self, fault: Fault) -> bool {
        let fault = match fault {
            Fault::RateLimited => FakeFault::RateLimited,
            Fault::ServerError => FakeFault::ServerError,
            Fault::TruncatedBody => FakeFault::TruncatedBody,
            Fault::ClosedEarly => FakeFault::ClosedEarly,
            Fault::ExpiredCredential => FakeFault::ExpiredSession,
            // No change feed, no cursors.
            Fault::CursorReset => return false,
        };
        self.server.inject(fault);
        true
    }
}

cloud_source::conformance_suite!(fake, FakeFixture::start());

/// The live session from the environment, checked with Apple.
enum Live {
    Absent,
    Expired,
    Ready(StoredSession),
}

fn live_session() -> Live {
    let Ok(raw) = std::env::var(LIVE_SESSION_VAR) else {
        return Live::Absent;
    };
    let session = StoredSession::from_json(&raw)
        .unwrap_or_else(|_| panic!("{LIVE_SESSION_VAR} does not hold a stored iCloud session"));
    let account = ICloudDrive::new(
        ICloudConfig::apple(),
        Arc::new(StaticCredential(session.credential())),
    )
    .unwrap();
    match account.stat(&account.root()) {
        Ok(_) => Live::Ready(session),
        Err(Error::Unauthorized) => Live::Expired,
        Err(error) => panic!("the live iCloud session could not be checked: {error}"),
    }
}

/// A live account, isolated in a fresh `conformance-<uuid>` folder that is
/// trashed on drop.
struct LiveFixture {
    credentials: Arc<SwitchableCredential>,
    source: ICloudDrive,
    remote: ICloudDrive,
    account: ICloudDrive,
    folder: ItemId,
}

impl LiveFixture {
    fn from_env() -> Option<Self> {
        let Live::Ready(session) = live_session() else {
            return None;
        };
        let shared = || StaticCredential(session.credential());
        let account = ICloudDrive::new(ICloudConfig::apple(), Arc::new(shared())).unwrap();
        let name = format!("conformance-{}", uuid::Uuid::new_v4().simple());
        let folder = account
            .create_folder(&account.root(), &name)
            .expect("create the run's folder")
            .id;
        let mut scoped = ICloudConfig::apple();
        scoped.root = folder.clone();
        let credentials = Arc::new(SwitchableCredential::new(Box::new(shared())));
        let source = ICloudDrive::new(scoped.clone(), Arc::clone(&credentials) as _).unwrap();
        let remote = ICloudDrive::new(scoped, Arc::new(shared())).unwrap();
        Some(Self {
            credentials,
            source,
            remote,
            account,
            folder,
        })
    }
}

impl Drop for LiveFixture {
    fn drop(&mut self) {
        let _ = self.account.delete(&self.folder, None);
    }
}

impl Fixture for LiveFixture {
    fn source(&self) -> &dyn CloudSource {
        &self.source
    }

    fn remote(&self) -> &dyn CloudSource {
        &self.remote
    }

    fn credentials(&self) -> &SwitchableCredential {
        &self.credentials
    }
}

cloud_source::conformance_suite!(
    live_suite,
    match LiveFixture::from_env() {
        Some(fixture) => fixture,
        None => return,
    }
);

mod live {
    use super::*;

    /// Fails with [`REAUTH_NEEDED`] when the live session needs a person;
    /// any other failure here is breakage.
    #[test]
    fn session_is_current() {
        match live_session() {
            Live::Absent | Live::Ready(_) => {}
            Live::Expired => panic!(
                "{REAUTH_NEEDED}: the iCloud session in {LIVE_SESSION_VAR} expired; \
                 re-auth needed (sign in with a verification code and update the secret)"
            ),
        }
    }

    /// Reports whether folder etags change when something two levels below
    /// changes. Informational: it decides whether iCloud could declare
    /// recursive folder revisions, and never fails on the answer.
    #[test]
    fn folder_etag_semantics() {
        let Some(fixture) = LiveFixture::from_env() else {
            return;
        };
        let source = &fixture.source;
        let outer = source.create_folder(&source.root(), "outer").unwrap();
        let inner = source.create_folder(&outer.id, "inner").unwrap();
        let file = cloud_source::upload(
            source,
            UploadTarget::Create {
                parent: inner.id,
                name: "f.txt".into(),
            },
            2,
            |_| Ok(Box::new(std::io::Cursor::new(b"v1".to_vec()))),
        )
        .unwrap();
        let before = source.stat(&outer.id).unwrap().revision;
        cloud_source::upload(
            source,
            UploadTarget::Replace {
                id: file.id,
                expect: None,
            },
            7,
            |_| Ok(Box::new(std::io::Cursor::new(b"version".to_vec()))),
        )
        .unwrap();
        let after = source.stat(&outer.id).unwrap().revision;
        eprintln!(
            "iCloud folder etags {} when a grandchild changes",
            if before == after {
                "stay the same"
            } else {
                "change"
            }
        );
    }
}

/// iCloud specifics on the fake, beyond the shared suite.
mod fake_specifics {
    use cloud_source_icloud::fake::Options;

    use super::*;

    fn signed_in(server: &FakeICloud) -> ICloudDrive {
        let session = server.session();
        ICloudDrive::new(
            server.config(),
            Arc::new(StaticCredential(session.credential())),
        )
        .unwrap()
    }

    fn put(source: &ICloudDrive, target: UploadTarget, bytes: &[u8]) -> cloud_source::Item {
        let owned = bytes.to_vec();
        cloud_source::upload(source, target, bytes.len() as u64, move |_| {
            Ok(Box::new(std::io::Cursor::new(owned.clone())))
        })
        .unwrap()
    }

    fn read_all(source: &ICloudDrive, id: &ItemId) -> Vec<u8> {
        let mut out = Vec::new();
        source
            .read(id, None, None)
            .unwrap()
            .body
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    #[test]
    fn decomposed_names_are_listed_composed() {
        let server = FakeICloud::start();
        let source = signed_in(&server);
        put(
            &source,
            UploadTarget::Create {
                parent: source.root(),
                name: "café.txt".into(),
            },
            b"x",
        );
        let listed = source.list(&source.root()).unwrap();
        assert_eq!(listed[0].name, "café.txt");
        let clash = cloud_source::upload(
            &source,
            UploadTarget::Create {
                parent: source.root(),
                name: "CAFE\u{301}.TXT".into(),
            },
            1,
            |_| Ok(Box::new(std::io::Cursor::new(b"y".to_vec()))),
        );
        assert!(matches!(clash, Err(cloud_source::Error::AlreadyExists)));
    }

    #[test]
    fn empty_files_round_trip() {
        let server = FakeICloud::start();
        let source = signed_in(&server);
        let item = put(
            &source,
            UploadTarget::Create {
                parent: source.root(),
                name: "empty".into(),
            },
            b"",
        );
        assert_eq!(item.size, Some(0));
        assert!(read_all(&source, &item.id).is_empty());
        assert_eq!(
            server.stats().full_downloads + server.stats().ranged_downloads,
            0
        );
    }

    #[test]
    fn replaces_settle_when_apple_only_adds_documents() {
        let server = FakeICloud::with(Options {
            replace_in_place: false,
            ..Options::default()
        });
        let source = signed_in(&server);
        let first = put(
            &source,
            UploadTarget::Create {
                parent: source.root(),
                name: "doc.txt".into(),
            },
            b"one",
        );
        let second = put(
            &source,
            UploadTarget::Replace {
                id: first.id.clone(),
                expect: first.revision.clone(),
            },
            b"two!",
        );
        assert_eq!(second.name, "doc.txt");
        assert_eq!(server.file("doc.txt").as_deref(), Some(&b"two!"[..]));
        assert_eq!(server.names(""), vec!["doc.txt".to_owned()]);
        assert!(matches!(
            source.stat(&first.id),
            Err(cloud_source::Error::NotFound)
        ));
    }

    #[test]
    fn folders_list_in_batches() {
        let server = FakeICloud::start();
        let source = signed_in(&server);
        let folders: Vec<ItemId> = (0..40)
            .map(|i| {
                source
                    .create_folder(&source.root(), &format!("f{i}"))
                    .unwrap()
                    .id
            })
            .collect();
        let before = server.stats().listings;
        let listed = source.list_folders(&folders).unwrap();
        assert_eq!(listed.len(), 40);
        assert!(listed.iter().all(|(_, children)| children.is_empty()));
        assert_eq!(
            server.stats().listings - before,
            2,
            "32 folders per request"
        );
    }

    #[test]
    fn ranged_reads_are_served_ranged() {
        let server = FakeICloud::start();
        let source = signed_in(&server);
        let item = put(
            &source,
            UploadTarget::Create {
                parent: source.root(),
                name: "r.bin".into(),
            },
            b"0123456789",
        );
        let mut body = Vec::new();
        source
            .read(&item.id, Some(2..5), None)
            .unwrap()
            .body
            .read_to_end(&mut body)
            .unwrap();
        assert_eq!(body, b"234");
        assert_eq!(server.stats().ranged_downloads, 1);
    }

    #[test]
    fn advanced_data_protection_mid_session_is_a_rejected_session() {
        let server = FakeICloud::start();
        let source = signed_in(&server);
        source.list(&source.root()).unwrap();
        server.set_advanced_data_protection(true);
        assert!(matches!(
            source.list(&source.root()),
            Err(cloud_source::Error::Unauthorized)
        ));
    }
}
