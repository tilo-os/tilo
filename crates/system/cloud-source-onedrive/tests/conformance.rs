//! The cloud-source conformance suite for `OneDrive`, in two layers:
//!
//! - the in-process fake Graph drive (always): consumer `OneDrive` mounted
//!   at the drive root and at a folder, `OneDrive` for Business mounted at a
//!   folder;
//! - a live account when `TILO_LIVE_ONEDRIVE_CLIENT_ID` and
//!   `TILO_LIVE_ONEDRIVE_REFRESH_TOKEN` are set (optional:
//!   `TILO_LIVE_ONEDRIVE_TENANT`, default `common`;
//!   `TILO_LIVE_ONEDRIVE_CLIENT_SECRET`; `TILO_LIVE_ONEDRIVE_DRIVE_ID` for a
//!   `SharePoint` library or another drive; `TILO_LIVE_ONEDRIVE_SCOPE`).
//!   Each check works in its own `conformance-<uuid>` folder, deleted
//!   afterwards; skipped when the variables are absent.
//!
//! There is no container layer: Graph has no self-hostable server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test fixtures"
)]

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use cloud_source::conformance::{Fault, Fixture, SwitchableCredential};
use cloud_source::{CloudSource, Credential, CredentialSource, Error, ItemId, StaticCredential};
use cloud_source_onedrive::fake::{FakeFault, FakeGraph, TOKEN};
use cloud_source_onedrive::{OneDriveConfig, OneDriveSource, RootSelector, paths};
use zeroize::Zeroizing;

fn bearer(token: &str) -> Credential {
    Credential::Bearer {
        token: Zeroizing::new(token.to_owned()),
        expires: None,
    }
}

/// The in-process fake drive.
struct FakeFixture {
    server: FakeGraph,
    credentials: Arc<SwitchableCredential>,
    source: OneDriveSource,
    remote: OneDriveSource,
}

impl FakeFixture {
    /// Mount the drive root (`folder` false) or a fresh folder next to
    /// unrelated content that must stay invisible.
    fn start(server: FakeGraph, folder: bool) -> Self {
        let root = if folder {
            server.put("Elsewhere/outside.txt", b"not mounted");
            RootSelector::Item(server.mkdirs("Mounts/conformance"))
        } else {
            RootSelector::DriveRoot
        };
        let config = server.config(root);
        let credentials = Arc::new(SwitchableCredential::new(Box::new(StaticCredential(
            bearer(TOKEN),
        ))));
        let source = OneDriveSource::connect(&config, Arc::clone(&credentials) as _).unwrap();
        let remote = OneDriveSource::new(
            source.mount().clone(),
            Arc::new(StaticCredential(bearer(TOKEN))),
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

impl Drop for FakeFixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            assert_eq!(
                self.server.stats().bearer_leaks,
                0,
                "the access token went to a pre-authenticated URL"
            );
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
            Fault::ExpiredCredential => self.server.inject(FakeFault::ExpiredCredential),
            Fault::CursorReset => self.server.reset_cursors(),
        }
        true
    }

    fn put_native_remote(&self, name: &str) -> bool {
        self.server
            .put_notebook(self.source.mount().root.as_str(), name)
            .is_some()
    }
}

cloud_source::conformance_suite!(
    fake_personal_drive_root,
    FakeFixture::start(FakeGraph::personal(), false)
);
cloud_source::conformance_suite!(
    fake_personal_folder,
    FakeFixture::start(FakeGraph::personal(), true)
);
cloud_source::conformance_suite!(
    fake_business_folder,
    FakeFixture::start(FakeGraph::business(), true)
);

/// A refresh-token credential against the Microsoft identity platform, as
/// Settings would hold it.
struct RefreshingCredential {
    client: reqwest::blocking::Client,
    token_url: String,
    client_id: String,
    client_secret: Option<String>,
    scope: String,
    state: Mutex<TokenState>,
}

struct TokenState {
    refresh_token: Zeroizing<String>,
    access: Option<(Zeroizing<String>, Instant)>,
}

impl RefreshingCredential {
    fn from_env() -> Option<Self> {
        let var = |name: &str| {
            std::env::var(format!("TILO_LIVE_ONEDRIVE_{name}"))
                .ok()
                .filter(|v| !v.is_empty())
        };
        let client_id = var("CLIENT_ID")?;
        let refresh_token = var("REFRESH_TOKEN")?;
        let tenant = var("TENANT").unwrap_or_else(|| "common".to_owned());
        Some(Self {
            client: tilo_net::http::blocking_client()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap(),
            token_url: format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
            client_id,
            client_secret: var("CLIENT_SECRET"),
            scope: var("SCOPE").unwrap_or_else(|| "Files.ReadWrite.All offline_access".to_owned()),
            state: Mutex::new(TokenState {
                refresh_token: Zeroizing::new(refresh_token),
                access: None,
            }),
        })
    }

    fn refresh(&self, state: &mut TokenState) -> cloud_source::Result<Zeroizing<String>> {
        let mut form = vec![
            ("client_id", self.client_id.clone()),
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", state.refresh_token.to_string()),
            ("scope", self.scope.clone()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        let body: String = form
            .iter()
            .map(|(key, value)| format!("{key}={}", paths::encode_segment(value)))
            .collect::<Vec<_>>()
            .join("&");
        let response = self
            .client
            .post(&self.token_url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .map_err(|_| Error::Offline)?;
        if !response.status().is_success() {
            return Err(Error::Unauthorized);
        }
        let text = response.text().map_err(|_| Error::Offline)?;
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| Error::InvalidResponse("token response"))?;
        let token = json["access_token"]
            .as_str()
            .ok_or(Error::InvalidResponse("access_token"))?
            .to_owned();
        if let Some(rotated) = json["refresh_token"].as_str() {
            state.refresh_token = Zeroizing::new(rotated.to_owned());
        }
        let lifetime = json["expires_in"]
            .as_u64()
            .unwrap_or(3600)
            .saturating_sub(120);
        let token = Zeroizing::new(token);
        state.access = Some((
            token.clone(),
            Instant::now() + Duration::from_secs(lifetime),
        ));
        Ok(token)
    }
}

impl CredentialSource for RefreshingCredential {
    fn credential(&self) -> cloud_source::Result<Credential> {
        let mut state = self.state.lock().unwrap();
        let token = match &state.access {
            Some((token, until)) if Instant::now() < *until => token.clone(),
            _ => self.refresh(&mut state)?,
        };
        drop(state);
        Ok(Credential::Bearer {
            token,
            expires: Some(SystemTime::now() + Duration::from_secs(60)),
        })
    }

    fn rejected(&self) {
        self.state.lock().unwrap().access = None;
    }
}

/// One refreshing credential shared by every live check.
struct Shared(Arc<RefreshingCredential>);

impl CredentialSource for Shared {
    fn credential(&self) -> cloud_source::Result<Credential> {
        self.0.credential()
    }

    fn rejected(&self) {
        self.0.rejected();
    }
}

fn live_credential() -> Option<Arc<RefreshingCredential>> {
    static CREDENTIAL: OnceLock<Option<Arc<RefreshingCredential>>> = OnceLock::new();
    CREDENTIAL
        .get_or_init(|| RefreshingCredential::from_env().map(Arc::new))
        .clone()
}

/// A live account, isolated in a fresh `conformance-<uuid>` folder below
/// the drive root.
struct LiveFixture {
    credentials: Arc<SwitchableCredential>,
    source: OneDriveSource,
    remote: OneDriveSource,
    account: OneDriveSource,
    folder: ItemId,
}

impl LiveFixture {
    fn from_env() -> Option<Self> {
        let shared = live_credential()?;
        let mut config = match std::env::var("TILO_LIVE_ONEDRIVE_DRIVE_ID") {
            Ok(id) if !id.is_empty() => OneDriveConfig::drive(id),
            _ => OneDriveConfig::me(),
        };
        let account =
            OneDriveSource::connect(&config, Arc::new(Shared(Arc::clone(&shared)))).unwrap();
        let name = format!("conformance-{}", uuid::Uuid::new_v4().simple());
        let folder = account
            .create_folder(&account.root(), &name)
            .expect("create the run's folder")
            .id;
        config.root = RootSelector::Item(folder.as_str().to_owned());
        let credentials = Arc::new(SwitchableCredential::new(Box::new(Shared(Arc::clone(
            &shared,
        )))));
        let source = OneDriveSource::connect(&config, Arc::clone(&credentials) as _).unwrap();
        let remote = OneDriveSource::new(source.mount().clone(), Arc::new(Shared(shared))).unwrap();
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
    live,
    match LiveFixture::from_env() {
        Some(fixture) => fixture,
        None => return,
    }
);
