//! The cloud-source conformance suite for `WebDAV` and Nextcloud, in all
//! three layers:
//!
//! - in-process fakes (always): plain `WebDAV`, `WebDAV` with
//!   `sync-collection`, Nextcloud;
//! - real servers in containers (`crates/system/cloud-source-dav/tests/servers.sh`
//!   starts them and prints `TILO_TEST_WEBDAV_*` / `TILO_TEST_NEXTCLOUD_*`;
//!   t's `cloud-servers` check);
//! - live accounts (`TILO_LIVE_WEBDAV_*` / `TILO_LIVE_NEXTCLOUD_*` from t's
//!   secrets, nightly in its `cloud-live` check).
//!
//! Server layers are skipped when their variables are absent, unless
//! `TILO_REQUIRE_CLOUD_SERVERS=1`; `tools/ci/cloud-conformance.sh` fails the
//! checks instead.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures"
)]

use std::sync::Arc;

use cloud_source::conformance::{Fault, Fixture, SwitchableCredential};
use cloud_source::{CloudSource, Credential, Error, ItemId, StaticCredential};
use cloud_source_dav::fake::{FakeDav, FakeFault, PASSWORD, USER};
use cloud_source_dav::{DavConfig, DavSource};
use zeroize::Zeroizing;

fn basic(user: &str, password: &str) -> Credential {
    Credential::Basic {
        username: user.to_owned(),
        password: Zeroizing::new(password.to_owned()),
    }
}

/// An in-process fake server.
struct FakeFixture {
    server: FakeDav,
    credentials: Arc<SwitchableCredential>,
    source: DavSource,
    remote: DavSource,
}

impl FakeFixture {
    fn start(server: FakeDav) -> Self {
        let credentials = Arc::new(SwitchableCredential::new(Box::new(StaticCredential(
            basic(USER, PASSWORD),
        ))));
        let source = DavSource::new(server.config(), Arc::clone(&credentials) as _).unwrap();
        source.probe().unwrap();
        let remote = DavSource::new(
            server.config(),
            Arc::new(StaticCredential(basic(USER, PASSWORD))),
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
        match fault {
            Fault::RateLimited => self.server.inject(FakeFault::RateLimited),
            Fault::ServerError => self.server.inject(FakeFault::ServerError),
            Fault::TruncatedBody => self.server.inject(FakeFault::TruncatedBody),
            Fault::ClosedEarly => self.server.inject(FakeFault::ClosedEarly),
            Fault::ExpiredCredential => self.server.inject(FakeFault::ExpiredCredential),
            Fault::CursorReset => self.server.reset_cursors(),
        }
        true
    }
}

cloud_source::conformance_suite!(fake_webdav, FakeFixture::start(FakeDav::generic()));
cloud_source::conformance_suite!(
    fake_webdav_sync_collection,
    FakeFixture::start(FakeDav::generic_with_sync_collection())
);
cloud_source::conformance_suite!(fake_nextcloud, FakeFixture::start(FakeDav::nextcloud()));

/// A listing that fails names its cause instead of collapsing to `Offline`,
/// so a failed conformance case says what the server did: it reset the
/// connection, cut the body short, or answered something that is not a
/// `multistatus`.
#[test]
fn listing_failures_name_their_cause() {
    let fixture = FakeFixture::start(FakeDav::generic());
    let source = fixture.source();
    let root = source.root();
    // The fake cuts a body in half, so leaving only the root behind makes
    // each cut land inside the document rather than after it.
    fixture.server.clear();
    source.list(&root).expect("the fake lists the root");

    // A body cut short after the head announced its full length.
    fixture.server.inject(FakeFault::TruncatedListing);
    let error = source.list(&root).expect_err("a truncated listing fails");
    assert!(
        matches!(&error, Error::InvalidResponse(detail) if detail.contains("cut short")),
        "a truncated listing names its cause: {error:?}"
    );
    assert!(
        error.is_transient(),
        "a cut body is worth retrying: {error:?}"
    );

    // A body the server cut off by closing the connection, with no length
    // to check it against: the document stops mid-way, which is what a
    // reader sees. The listing still says why it could not be used.
    fixture.server.inject(FakeFault::ClosedListing);
    let error = source.list(&root).expect_err("a closed listing fails");
    assert!(
        matches!(&error, Error::InvalidResponse(detail) if detail.contains("malformed")),
        "a closed body names its cause: {error:?}"
    );

    // A 200 that is an HTML page, not a listing.
    fixture.server.inject(FakeFault::NotMultistatus);
    let error = source
        .list(&root)
        .expect_err("a non-multistatus answer fails");
    assert!(
        matches!(&error, Error::InvalidResponse(detail) if detail.contains("not multistatus")),
        "a non-multistatus answer names its cause: {error:?}"
    );
    assert!(
        !error.is_transient(),
        "a server that answers wrongly is not offline: {error:?}"
    );

    // A server that cannot be reached at all.
    fixture.server.set_offline(true);
    let error = source.list(&root).expect_err("an unreachable server fails");
    fixture.server.set_offline(false);
    assert!(
        matches!(&error, Error::InvalidResponse("transport")),
        "an unreachable server names the transport: {error:?}"
    );
    assert!(
        error.is_transient(),
        "an unreachable server is worth retrying: {error:?}"
    );

    source.list(&root).expect("the fake recovers");
}

/// A sync REPORT that fails names its cause too, and a server whose REPORT
/// is answered but unusable has no change feed rather than being offline.
#[test]
fn change_feed_failures_name_their_cause() {
    let fixture = FakeFixture::start(FakeDav::generic_with_sync_collection());
    let source = fixture.source();
    let page = source.changes(None).expect("the fake reports changes");
    let cursor = page.cursor;

    // A REPORT answered with an ordinary 200 and an HTML page: the cause is
    // the status, not a body that was never a report.
    fixture.server.inject(FakeFault::NotMultistatus);
    let error = source
        .changes(Some(&cursor))
        .expect_err("a non-multistatus report fails");
    assert!(
        matches!(&error, Error::Status(200)),
        "a non-multistatus answer names its cause: {error:?}"
    );

    // A 207 whose body is not a report: the body keeps its own cause.
    fixture.server.inject(FakeFault::NotMultistatus);
    let client = tilo_dav::DavClient::new(&tilo_dav::Options::default())
        .expect("dav client")
        .with_credentials(tilo_dav::Credentials::new(USER, PASSWORD));
    let report = client
        .report(
            fixture.server.root_url(),
            tilo_dav::Depth::Zero,
            "<sync/>".to_owned(),
        )
        .expect("the fake answers");
    assert_eq!(report.status(), tilo_dav::StatusCode::OK);
    assert!(
        matches!(
            report.multistatus(&tilo_dav::Limits::default()),
            Err(tilo_dav::Error::Xml(tilo_dav::XmlError::NotMultistatus))
        ),
        "a page that is not a report is not a multistatus"
    );

    // Probing asks the same REPORT: a server that answers it with something
    // that is not a report offers no feed, and says so rather than failing.
    // The source is built by hand, because the fixture probes at start-up.
    let quiet = FakeDav::generic_with_sync_collection();
    quiet.inject(FakeFault::NotMultistatus);
    let quiet_source = DavSource::new(
        quiet.config(),
        Arc::new(StaticCredential(basic(USER, PASSWORD))),
    )
    .unwrap();
    assert!(
        !quiet_source
            .probe()
            .expect("a wrong answer is not an outage"),
        "a server that answers an HTML page has no change feed"
    );
    quiet_source
        .list(&quiet_source.root())
        .expect("the server still lists");
}

/// A reset while probing is an outage, not a server without a change feed.
#[test]
fn probing_a_reset_server_fails() {
    let fixture = FakeFixture::start(FakeDav::generic());
    fixture.server.set_offline(true);
    let error = fixture
        .source
        .probe()
        .expect_err("an unreachable server fails");
    fixture.server.set_offline(false);
    assert!(
        matches!(&error, Error::InvalidResponse("transport")),
        "a reset names the transport: {error:?}"
    );
}

/// A real server (a container in CI, or a live account), isolated in a
/// fresh `conformance-<uuid>` folder that is deleted on drop.
struct ServerFixture {
    credentials: Arc<SwitchableCredential>,
    source: DavSource,
    remote: DavSource,
    account_root: DavSource,
    folder: ItemId,
    /// The Nextcloud trash bin of the account, when the root is one.
    trash: Option<DavSource>,
}

impl ServerFixture {
    /// From `<prefix>_URL`, `<prefix>_USER`, `<prefix>_PASSWORD`; `None`
    /// (skip) when unset. An empty value is unset: a workflow passes a
    /// secret that is not configured as an empty string.
    fn from_env(prefix: &str, nextcloud: bool) -> Option<Self> {
        let var = |name: &str| {
            std::env::var(format!("{prefix}_{name}"))
                .ok()
                .filter(|value| !value.is_empty())
        };
        let Some(url) = var("URL") else {
            assert!(
                std::env::var("TILO_REQUIRE_CLOUD_SERVERS").as_deref() != Ok("1")
                    || prefix.starts_with("TILO_LIVE"),
                "{prefix}_URL is required on this lane"
            );
            return None;
        };
        let user = var("USER").unwrap_or_default();
        let password = var("PASSWORD").unwrap_or_default();
        let url = tilo_dav::Url::parse(&url).expect("server url");
        let base = if nextcloud {
            DavConfig::nextcloud(url, &user).expect("nextcloud config")
        } else {
            DavConfig::generic(url)
        };
        // A Nextcloud files root (either suite may point at one) keeps every
        // deleted run folder in that account's trash bin.
        let trash = base
            .root
            .path()
            .strip_prefix("/remote.php/dav/files/")
            .and_then(|rest| rest.strip_suffix('/'))
            .filter(|account| !account.contains('/'))
            .map(|account| {
                let root = base
                    .root
                    .join(&format!("/remote.php/dav/trashbin/{account}/trash/"))
                    .expect("trash url");
                DavSource::new(
                    DavConfig::generic(root),
                    Arc::new(StaticCredential(basic(&user, &password))),
                )
                .unwrap()
            });
        let fixed = || Arc::new(StaticCredential(basic(&user, &password)));
        let account_root = DavSource::new(base.clone(), fixed()).unwrap();
        let folder_name = format!("conformance-{}", uuid::Uuid::new_v4().simple());
        let folder = account_root
            .create_folder(&account_root.root(), &folder_name)
            .expect("create the run's folder")
            .id;
        let mut scoped = base;
        scoped.root = cloud_source_dav::paths::url_for(&scoped.root, &folder, true).unwrap();
        let credentials = Arc::new(SwitchableCredential::new(Box::new(StaticCredential(
            basic(&user, &password),
        ))));
        let source = DavSource::new(scoped.clone(), Arc::clone(&credentials) as _).unwrap();
        source.probe().expect("probe");
        let remote = DavSource::new(scoped, fixed()).unwrap();
        Some(Self {
            credentials,
            source,
            remote,
            account_root,
            folder,
            trash,
        })
    }
}

impl Drop for ServerFixture {
    fn drop(&mut self) {
        let _ = self.account_root.delete(&self.folder, None);
        // Nextcloud keeps everything the suite deletes in the account's
        // trash bin; the account is the suite's own, so empty it.
        if let Some(trash) = &self.trash
            && let Ok(items) = trash.list(&trash.root())
        {
            for item in items {
                let _ = trash.delete(&item.id, None);
            }
        }
    }
}

impl Fixture for ServerFixture {
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

macro_rules! server_suite {
    ($module:ident, $prefix:literal, $nextcloud:literal) => {
        cloud_source::conformance_suite!(
            $module,
            match ServerFixture::from_env($prefix, $nextcloud) {
                Some(fixture) => fixture,
                None => return,
            }
        );
    };
}

server_suite!(container_webdav, "TILO_TEST_WEBDAV", false);
server_suite!(container_nextcloud, "TILO_TEST_NEXTCLOUD", true);
server_suite!(live_webdav, "TILO_LIVE_WEBDAV", false);
server_suite!(live_nextcloud, "TILO_LIVE_NEXTCLOUD", true);
