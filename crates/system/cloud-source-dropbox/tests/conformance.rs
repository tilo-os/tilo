//! The cloud-source conformance suite for Dropbox:
//!
//! - in-process fakes (always): the whole account, a mounted folder whose
//!   configured path differs in case from the real one, and a team member
//!   rooted in the team space;
//! - a live account when `TILO_LIVE_DROPBOX_*` is set (CI secrets, on a
//!   schedule), isolated in a fresh `conformance-<uuid>` folder that is
//!   deleted afterwards. Either `TILO_LIVE_DROPBOX_TOKEN` (a short-lived
//!   access token) or `TILO_LIVE_DROPBOX_REFRESH_TOKEN` with
//!   `TILO_LIVE_DROPBOX_APP_KEY` (and `TILO_LIVE_DROPBOX_APP_SECRET` for
//!   confidential apps).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures"
)]

use std::sync::{Arc, Mutex};

use cloud_source::conformance::{Fault, Fixture, SwitchableCredential};
use cloud_source::{
    CloudSource, Credential, CredentialSource, Error, ItemId, Result, StaticCredential,
};
use cloud_source_dropbox::fake::{FakeDropbox, FakeFault, TEAM_NAMESPACE, TOKEN};
use cloud_source_dropbox::{DropboxConfig, DropboxSource};
use zeroize::Zeroizing;

fn bearer(token: &str) -> Credential {
    Credential::Bearer {
        token: Zeroizing::new(token.to_owned()),
        expires: None,
    }
}

/// An in-process fake Dropbox.
struct FakeFixture {
    server: FakeDropbox,
    credentials: Arc<SwitchableCredential>,
    source: DropboxSource,
    remote: DropboxSource,
}

impl FakeFixture {
    /// The source mounts `configured`; `actual` is the folder as it exists
    /// on the server (they may differ in case).
    fn start(server: FakeDropbox, actual: &str, configured: &str, team: bool) -> Self {
        if !actual.is_empty() {
            server.mkdir(actual);
        }
        let mut config = server.config(configured);
        if team {
            config.path_root = Some(TEAM_NAMESPACE.to_owned());
        }
        let credentials = Arc::new(SwitchableCredential::new(Box::new(StaticCredential(
            bearer(TOKEN),
        ))));
        let source = DropboxSource::new(config.clone(), Arc::clone(&credentials) as _).unwrap();
        let remote = DropboxSource::new(config, Arc::new(StaticCredential(bearer(TOKEN)))).unwrap();
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

cloud_source::conformance_suite!(
    fake_account,
    FakeFixture::start(FakeDropbox::start(), "", "", false)
);
cloud_source::conformance_suite!(
    fake_folder,
    FakeFixture::start(
        FakeDropbox::start(),
        "/Mounts/Conformance-Root",
        "/mounts/conformance-root",
        false
    )
);
cloud_source::conformance_suite!(
    fake_team_space,
    FakeFixture::start(FakeDropbox::team(), "/Team Folder", "/Team Folder", true)
);

/// Exchanges a refresh token for access tokens at Dropbox's token
/// endpoint, as Settings does for a signed-in account.
struct RefreshingCredential {
    app_key: String,
    app_secret: Option<String>,
    refresh_token: Zeroizing<String>,
    access: Mutex<Option<Zeroizing<String>>>,
}

fn form_escape(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

impl RefreshingCredential {
    fn refresh(&self) -> Result<Zeroizing<String>> {
        let mut body = format!(
            "grant_type=refresh_token&refresh_token={}&client_id={}",
            form_escape(&self.refresh_token),
            form_escape(&self.app_key)
        );
        if let Some(secret) = &self.app_secret {
            body.push_str("&client_secret=");
            body.push_str(&form_escape(secret));
        }
        let client = tilo_net::http::blocking_client()
            .build()
            .map_err(|_| Error::Offline)?;
        let response = client
            .post("https://api.dropboxapi.com/oauth2/token")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .map_err(|_| Error::Offline)?;
        if !response.status().is_success() {
            return Err(Error::Unauthorized);
        }
        let text = response.text().map_err(|_| Error::Offline)?;
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| Error::Unauthorized)?;
        value
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .map(|token| Zeroizing::new(token.to_owned()))
            .ok_or(Error::Unauthorized)
    }
}

impl CredentialSource for RefreshingCredential {
    fn credential(&self) -> Result<Credential> {
        let token = {
            let mut access = self.access.lock().unwrap();
            if access.is_none() {
                *access = Some(self.refresh()?);
            }
            access.clone().ok_or(Error::Unauthorized)?
        };
        Ok(Credential::Bearer {
            token,
            expires: None,
        })
    }

    fn rejected(&self) {
        *self.access.lock().unwrap() = None;
    }
}

fn live_credentials() -> Option<Arc<dyn CredentialSource>> {
    let var = |name: &str| std::env::var(format!("TILO_LIVE_DROPBOX_{name}")).ok();
    if let Some(token) = var("TOKEN") {
        return Some(Arc::new(StaticCredential(bearer(&token))));
    }
    let refresh_token = var("REFRESH_TOKEN")?;
    let app_key = var("APP_KEY").expect("TILO_LIVE_DROPBOX_APP_KEY with a refresh token");
    Some(Arc::new(RefreshingCredential {
        app_key,
        app_secret: var("APP_SECRET"),
        refresh_token: Zeroizing::new(refresh_token),
        access: Mutex::new(None),
    }))
}

/// A live account, isolated in a fresh `conformance-<uuid>` folder that is
/// deleted on drop.
struct LiveFixture {
    credentials: Arc<SwitchableCredential>,
    source: DropboxSource,
    remote: DropboxSource,
    account: DropboxSource,
    folder: ItemId,
}

impl LiveFixture {
    fn from_env() -> Option<Self> {
        let shared = live_credentials()?;
        let account = DropboxSource::new(DropboxConfig::new(""), Arc::clone(&shared)).unwrap();
        let name = format!("conformance-{}", uuid::Uuid::new_v4().simple());
        let folder = account
            .create_folder(&account.root(), &name)
            .expect("create the run's folder")
            .id;
        let config = DropboxConfig::new(&format!("/{name}"));
        let credentials = Arc::new(SwitchableCredential::new(Box::new(ForwardCredential(
            Arc::clone(&shared),
        ))));
        let source = DropboxSource::new(config.clone(), Arc::clone(&credentials) as _).unwrap();
        let remote = DropboxSource::new(config, shared).unwrap();
        Some(Self {
            credentials,
            source,
            remote,
            account,
            folder,
        })
    }
}

/// Shares one refreshing credential between the fixture's clients.
struct ForwardCredential(Arc<dyn CredentialSource>);

impl CredentialSource for ForwardCredential {
    fn credential(&self) -> Result<Credential> {
        self.0.credential()
    }

    fn rejected(&self) {
        self.0.rejected();
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
