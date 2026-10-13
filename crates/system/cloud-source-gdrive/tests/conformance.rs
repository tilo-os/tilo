//! The cloud-source conformance suite for Google Drive:
//!
//! - in-process fakes (always): a folder in My Drive, the whole of a fresh
//!   My Drive (the `root` alias), and a folder in a shared drive;
//! - a live account when `TILO_LIVE_GDRIVE_TOKEN` (an `OAuth` refresh token)
//!   and `TILO_LIVE_GDRIVE_CLIENT_ID` (plus `TILO_LIVE_GDRIVE_CLIENT_SECRET`
//!   for client types that have one) are set; skipped otherwise. Each run
//!   works in its own `conformance-<uuid>` folder, deleted afterwards.
//!
//! There is no container layer: Drive has no self-hostable server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test fixtures"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use cloud_source::conformance::{Fault, Fixture, SwitchableCredential};
use cloud_source::{CloudSource, Credential, CredentialSource, Error, ItemId, StaticCredential};
use cloud_source_gdrive::fake::{FakeDrive, FakeFault, REMOTE_TOKEN, SOURCE_TOKEN};
use cloud_source_gdrive::{DriveConfig, GDrive};
use zeroize::Zeroizing;

fn bearer(token: &str) -> Credential {
    Credential::Bearer {
        token: Zeroizing::new(token.to_owned()),
        expires: None,
    }
}

fn fixed(token: &str) -> Arc<StaticCredential> {
    Arc::new(StaticCredential(bearer(token)))
}

/// Where in the fake Drive a run is rooted.
#[derive(Clone, Copy)]
enum Scope {
    /// A fresh folder in My Drive.
    Folder,
    /// The whole (empty) My Drive.
    MyDrive,
    /// A fresh folder in a shared drive.
    SharedDrive,
}

struct FakeFixture {
    server: FakeDrive,
    credentials: Arc<SwitchableCredential>,
    source: GDrive,
    remote: GDrive,
    folder: ItemId,
}

impl FakeFixture {
    fn start(scope: Scope) -> Self {
        let server = FakeDrive::start();
        let base = match scope {
            Scope::Folder | Scope::MyDrive => server.config(),
            Scope::SharedDrive => {
                let drive = server.add_shared_drive("Team");
                DriveConfig {
                    shared_drive: Some(drive),
                    ..server.config()
                }
            }
        };
        let config = match scope {
            Scope::MyDrive => base,
            Scope::Folder | Scope::SharedDrive => {
                let account = GDrive::new(base.clone(), fixed(REMOTE_TOKEN)).unwrap();
                let name = format!("conformance-{}", uuid::Uuid::new_v4().simple());
                let folder = account.create_folder(&account.root(), &name).unwrap();
                base.with_root(folder.id.0)
            }
        };
        let credentials = Arc::new(SwitchableCredential::new(Box::new(StaticCredential(
            bearer(SOURCE_TOKEN),
        ))));
        let source = GDrive::new(config.clone(), Arc::clone(&credentials) as _).unwrap();
        let remote = GDrive::new(config, fixed(REMOTE_TOKEN)).unwrap();
        let folder = remote.root();
        Self {
            server,
            credentials,
            source,
            remote,
            folder,
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
            Fault::RateLimited => self.server.inject(FakeFault::UserRateLimited),
            Fault::ServerError => self.server.inject(FakeFault::ServerError),
            Fault::TruncatedBody => self.server.inject(FakeFault::TruncatedBody),
            Fault::ClosedEarly => self.server.inject(FakeFault::ClosedEarly),
            Fault::ExpiredCredential => self.server.inject(FakeFault::ExpiredCredential),
            Fault::CursorReset => self.server.reset_cursors(),
        }
        true
    }

    fn put_native_remote(&self, name: &str) -> bool {
        self.server.put_native(
            self.folder.as_str(),
            name,
            "application/vnd.google-apps.document",
        );
        true
    }
}

cloud_source::conformance_suite!(fake_folder, FakeFixture::start(Scope::Folder));
cloud_source::conformance_suite!(fake_my_drive, FakeFixture::start(Scope::MyDrive));
cloud_source::conformance_suite!(fake_shared_drive, FakeFixture::start(Scope::SharedDrive));

/// Exchanges a refresh token for access tokens, the way Settings does.
struct RefreshingCredential {
    http: reqwest::blocking::Client,
    refresh_token: String,
    client_id: String,
    client_secret: Option<String>,
    cached: Mutex<Option<(String, SystemTime)>>,
}

impl RefreshingCredential {
    fn refresh(&self) -> cloud_source::Result<(String, SystemTime)> {
        let mut form = reqwest::Url::parse("https://oauth2.googleapis.com/token").unwrap();
        {
            let mut pairs = form.query_pairs_mut();
            pairs
                .append_pair("grant_type", "refresh_token")
                .append_pair("refresh_token", &self.refresh_token)
                .append_pair("client_id", &self.client_id);
            if let Some(secret) = &self.client_secret {
                pairs.append_pair("client_secret", secret);
            }
        }
        let body = form.query().unwrap_or_default().to_owned();
        let response = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .map_err(|_| Error::Offline)?;
        if !response.status().is_success() {
            return Err(Error::Unauthorized);
        }
        let bytes = response.bytes().map_err(|_| Error::Offline)?;
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| Error::InvalidResponse("token response"))?;
        let token = json["access_token"]
            .as_str()
            .ok_or(Error::InvalidResponse("access_token"))?
            .to_owned();
        let lifetime = json["expires_in"].as_u64().unwrap_or(3600);
        let expires = SystemTime::now() + Duration::from_secs(lifetime.saturating_sub(60));
        Ok((token, expires))
    }
}

impl CredentialSource for RefreshingCredential {
    fn credential(&self) -> cloud_source::Result<Credential> {
        let mut cached = self.cached.lock().unwrap();
        let fresh = match cached.as_ref() {
            Some((token, expires)) if *expires > SystemTime::now() => (token.clone(), *expires),
            _ => {
                let fresh = self.refresh()?;
                *cached = Some(fresh.clone());
                fresh
            }
        };
        drop(cached);
        Ok(Credential::Bearer {
            token: Zeroizing::new(fresh.0),
            expires: Some(fresh.1),
        })
    }

    fn rejected(&self) {
        *self.cached.lock().unwrap() = None;
    }
}

/// A live Google account, isolated in a fresh `conformance-<uuid>` folder
/// that is deleted (permanently) on drop.
struct LiveFixture {
    credentials: Arc<SwitchableCredential>,
    source: GDrive,
    remote: GDrive,
    account: GDrive,
    folder: ItemId,
}

impl LiveFixture {
    fn from_env() -> Option<Self> {
        let refresh_token = std::env::var("TILO_LIVE_GDRIVE_TOKEN").ok()?;
        let client_id = std::env::var("TILO_LIVE_GDRIVE_CLIENT_ID")
            .expect("TILO_LIVE_GDRIVE_CLIENT_ID goes with TILO_LIVE_GDRIVE_TOKEN");
        let client_secret = std::env::var("TILO_LIVE_GDRIVE_CLIENT_SECRET").ok();
        let credential = || RefreshingCredential {
            http: tilo_net::http::blocking_client().build().unwrap(),
            refresh_token: refresh_token.clone(),
            client_id: client_id.clone(),
            client_secret: client_secret.clone(),
            cached: Mutex::new(None),
        };
        let account = GDrive::new(
            DriveConfig {
                trash: false,
                ..DriveConfig::my_drive()
            },
            Arc::new(credential()),
        )
        .unwrap();
        let name = format!("conformance-{}", uuid::Uuid::new_v4().simple());
        let folder = account
            .create_folder(&account.root(), &name)
            .expect("create the run's folder")
            .id;
        let config = DriveConfig::my_drive().with_root(folder.0.clone());
        let credentials = Arc::new(SwitchableCredential::new(Box::new(credential())));
        let source = GDrive::new(config.clone(), Arc::clone(&credentials) as _).unwrap();
        let remote = GDrive::new(config, Arc::new(credential())).unwrap();
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

    fn put_native_remote(&self, name: &str) -> bool {
        self.remote
            .create_native_document(
                &self.remote.root(),
                name,
                "application/vnd.google-apps.document",
            )
            .expect("create a Google Doc");
        true
    }
}

cloud_source::conformance_suite!(
    live,
    match LiveFixture::from_env() {
        Some(fixture) => fixture,
        None => return,
    }
);
