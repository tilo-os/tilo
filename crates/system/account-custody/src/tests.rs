//! Custody end to end against the fake authorization server: sign-in,
//! publication, refresh when due and on rejection, a revoked refresh token
//! becoming "sign in again", and the daemon only ever seeing access tokens.

use std::path::Path;
use std::sync::{Arc, Mutex};

use accounts::{
    Account, AccountId, AccountStore, AccountSyncHealth, AccountSyncStatusStore, HandoffCredential,
    HandoffCredentialSource, HandoffStore, Secret, SecretVault,
};
use cloud_source::CredentialSource as _;
use tilo_oauth::fake::{FakeAuthorizationServer, browse};
use zeroize::Zeroizing;

use crate::icloud::{
    ICloudAuth, ICloudAuthError, ICloudChallenge, ICloudHeaders, ICloudSessionState, ICloudSignIn,
};
use crate::providers::{GOOGLE_DRIVE_TYPE, ICLOUD_DRIVE_TYPE, WEBDAV_TYPE};
use crate::{Custodian, CustodyError, OAuthCloud, icloud_secret, oauth_secret};

fn save(root: &Path, id: &AccountId, type_id: &str, secret: &Secret, config: serde_json::Value) {
    let mut vault = SecretVault::open_in(root);
    vault.put(id, secret);
    vault.try_save().unwrap();
    let mut store = AccountStore::load_in(root);
    let mut account = Account::new(id.clone(), type_id);
    account.display_name = "Test".into();
    account.config = config;
    store.upsert(account);
    store.save_in(root).unwrap();
}

fn health(root: &Path, id: &AccountId) -> Option<AccountSyncHealth> {
    AccountSyncStatusStore::load_in(root)
        .get(id)
        .map(|status| status.health)
}

fn sign_in(custodian: &Custodian) -> tilo_oauth::TokenSet {
    let authorization = custodian
        .begin_sign_in(OAuthCloud::GoogleDrive, None)
        .unwrap();
    let url = authorization.url().to_string();
    let browser = std::thread::spawn(move || browse(&url));
    let tokens = Custodian::finish_sign_in(authorization).unwrap();
    browser.join().unwrap().unwrap();
    tokens
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    server: FakeAuthorizationServer,
    custodian: Custodian,
    id: AccountId,
}

fn signed_in_drive(lifetime: u64) -> (Fixture, tilo_oauth::TokenSet) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tilo");
    let server = FakeAuthorizationServer::start();
    server.set_token_lifetime(lifetime);
    let custodian = Custodian::for_root(Some(root.clone())).with_provider(
        OAuthCloud::GoogleDrive,
        server.provider(),
        "drive-client",
    );
    let tokens = sign_in(&custodian);
    let id = AccountId::from("google-drive-1");
    save(
        &root,
        &id,
        GOOGLE_DRIVE_TYPE,
        &oauth_secret(&tokens),
        serde_json::json!({}),
    );
    custodian.signed_in(&id).unwrap();
    (
        Fixture {
            _dir: dir,
            root,
            server,
            custodian,
            id,
        },
        tokens,
    )
}

fn handed_off_token(root: &Path, id: &AccountId) -> String {
    match HandoffStore::open_in(root).read(id).unwrap() {
        Some(HandoffCredential::Bearer { token, .. }) => token.to_string(),
        other => panic!("expected a bearer handoff, got {other:?}"),
    }
}

fn vault_refresh_token(root: &Path, id: &AccountId) -> String {
    let secret = SecretVault::open_in(root).get(id).unwrap();
    let grant: serde_json::Value = serde_json::from_str(secret.token.as_deref().unwrap()).unwrap();
    grant["tokens"]["refresh_token"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn unreadable_credentials_are_storage_errors_not_missing_grants() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tilo");
    let id = AccountId::from("google-drive-1");
    let tokens = tilo_oauth::TokenSet::default();
    save(
        &root,
        &id,
        GOOGLE_DRIVE_TYPE,
        &oauth_secret(&tokens),
        serde_json::json!({}),
    );
    let sealed = accounts::vault_dir_in(&root).join("vault.enc");
    std::fs::write(&sealed, b"corrupt credentials").unwrap();
    let custodian = Custodian::for_root(Some(root.clone()));

    assert!(matches!(
        custodian.seal_oauth(&id, &tokens),
        Err(CustodyError::Storage(_))
    ));
    assert!(matches!(
        custodian.oauth_access_token(&id, GMAIL, false),
        Err(CustodyError::Storage(_))
    ));
    assert!(matches!(
        custodian.publish(&id, false),
        Err(CustodyError::Storage(_))
    ));
    assert_eq!(std::fs::read(&sealed).unwrap(), b"corrupt credentials");
    assert_eq!(health(&root, &id), None);
}

#[test]
fn sign_in_publishes_the_access_token_only() {
    let (fixture, tokens) = signed_in_drive(3600);
    assert_eq!(
        handed_off_token(&fixture.root, &fixture.id),
        tokens.access_token
    );
    assert_eq!(
        vault_refresh_token(&fixture.root, &fixture.id),
        tokens.refresh_token,
        "the refresh token stays in the vault"
    );
    assert_eq!(
        fixture.server.refreshes(),
        0,
        "a fresh token is not refreshed"
    );
}

#[test]
fn a_due_token_is_refreshed_before_it_is_handed_out() {
    // A one second lifetime is inside the refresh margin.
    let (fixture, tokens) = signed_in_drive(1);
    assert_eq!(fixture.server.refreshes(), 1, "signed_in already refreshed");
    let handed = handed_off_token(&fixture.root, &fixture.id);
    assert_ne!(handed, tokens.access_token);
    assert_eq!(
        vault_refresh_token(&fixture.root, &fixture.id),
        tokens.refresh_token
    );
}

#[test]
fn a_rejected_token_is_replaced_through_the_daemon_source() {
    let (fixture, tokens) = signed_in_drive(3600);
    let source = HandoffCredentialSource::new(
        fixture.id.clone(),
        HandoffStore::open_in(&fixture.root),
        Arc::new(fixture.custodian.clone()),
    );
    let bearer = |credential: cloud_source::Credential| match credential {
        cloud_source::Credential::Bearer { token, .. } => token.to_string(),
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(bearer(source.credential().unwrap()), tokens.access_token);
    source.rejected();
    let renewed = bearer(source.credential().unwrap());
    assert_ne!(renewed, tokens.access_token);
    assert_eq!(fixture.server.refreshes(), 1);
}

#[test]
fn a_revoked_refresh_token_moves_the_account_to_needs_authentication() {
    let (fixture, _tokens) = signed_in_drive(3600);
    fixture.server.revoke_all_refresh_tokens();
    assert_eq!(
        fixture.custodian.publish(&fixture.id, true),
        Err(CustodyError::NeedsAuthentication)
    );
    assert_eq!(
        health(&fixture.root, &fixture.id),
        Some(AccountSyncHealth::NeedsAuthentication)
    );
    assert!(
        HandoffStore::open_in(&fixture.root)
            .read(&fixture.id)
            .unwrap()
            .is_none(),
        "the rejected access token is withdrawn"
    );
    let source = HandoffCredentialSource::new(
        fixture.id.clone(),
        HandoffStore::open_in(&fixture.root),
        Arc::new(fixture.custodian.clone()),
    );
    assert!(matches!(
        source.credential(),
        Err(cloud_source::Error::Unauthorized)
    ));
}

#[test]
fn the_daemon_never_receives_a_refresh_token() {
    let (fixture, tokens) = signed_in_drive(3600);
    let source = HandoffCredentialSource::new(
        fixture.id.clone(),
        HandoffStore::open_in(&fixture.root),
        Arc::new(fixture.custodian.clone()),
    );
    let mut seen = Vec::new();
    for _ in 0..3 {
        source.rejected();
        let credential = source.credential().unwrap();
        let cloud_source::Credential::Bearer { token, .. } = &credential else {
            panic!("an OAuth account hands out bearer tokens only");
        };
        seen.push(token.to_string());
        assert!(!format!("{credential:?}").contains(token.as_str()));
    }
    let refresh = vault_refresh_token(&fixture.root, &fixture.id);
    assert_eq!(refresh, tokens.refresh_token);
    assert!(seen.iter().all(|token| token != &refresh));
    // Every handoff file, decrypted, carries nothing but an access token.
    let dir = accounts::handoff::handoff_dir_in(&fixture.root);
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "enc"))
        .collect();
    assert_eq!(entries.len(), 1, "one account, one handoff file");
    match HandoffStore::open_in(&fixture.root)
        .read(&fixture.id)
        .unwrap()
    {
        Some(HandoffCredential::Bearer { token, .. }) => {
            assert_ne!(token.as_str(), refresh);
            assert_eq!(Some(token.to_string()), seen.last().cloned());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_missing_client_id_is_not_configured_not_a_broken_flow() {
    let custodian = Custodian::for_root(Some(std::env::temp_dir().join("tilo-custody-unused")));
    match custodian.begin_sign_in(OAuthCloud::Dropbox, None) {
        Err(CustodyError::NotConfigured { provider, key }) => {
            assert_eq!(provider, "Dropbox");
            assert_eq!(key, "TILO_OAUTH_DROPBOX_CLIENT_ID");
        }
        Ok(authorization) => assert!(
            authorization
                .url()
                .starts_with("https://www.dropbox.com/oauth2/authorize?")
        ),
        Err(other) => panic!("unexpected {other:?}"),
    }
}

#[test]
fn revocation_on_removal_reaches_the_provider() {
    let (fixture, tokens) = signed_in_drive(3600);
    let account = AccountStore::load_in(&fixture.root)
        .get(&fixture.id)
        .cloned()
        .unwrap();
    let secret = SecretVault::open_in(&fixture.root)
        .get(&fixture.id)
        .unwrap();
    fixture.custodian.revoke(&account, &secret);
    assert_eq!(fixture.server.revoked(), vec![tokens.refresh_token]);
    assert!(
        HandoffStore::open_in(&fixture.root)
            .read(&fixture.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn webdav_hands_off_its_own_basic_credential() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tilo");
    let id = AccountId::from("webdav-1");
    save(
        &root,
        &id,
        WEBDAV_TYPE,
        &Secret::password("dav-password"),
        serde_json::json!({"base_url": "https://dav.example", "username": "ada"}),
    );
    Custodian::for_root(Some(root.clone()))
        .publish(&id, false)
        .unwrap();
    match HandoffStore::open_in(&root).read(&id).unwrap() {
        Some(HandoffCredential::Basic { username, password }) => {
            assert_eq!(username, "ada");
            assert_eq!(password.as_str(), "dav-password");
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// An `iCloud` provider that asks for a code, then hands out session
/// headers until told the session needs the two-factor step again.
#[derive(Default)]
pub(crate) struct FakeICloud {
    pub needs_reauth: Mutex<bool>,
    pub adp: bool,
}

impl ICloudAuth for FakeICloud {
    fn sign_in(&self, apple_id: &str, password: &str) -> Result<ICloudSignIn, ICloudAuthError> {
        if self.adp {
            return Err(ICloudAuthError::AdvancedDataProtection);
        }
        if password != "apple-password" {
            return Err(ICloudAuthError::InvalidCredentials);
        }
        Ok(ICloudSignIn::NeedsCode(ICloudChallenge(Zeroizing::new(
            format!("challenge:{apple_id}"),
        ))))
    }

    fn verify_code(
        &self,
        challenge: &ICloudChallenge,
        code: &str,
    ) -> Result<ICloudSessionState, ICloudAuthError> {
        if code != "123456" {
            return Err(ICloudAuthError::InvalidCode);
        }
        Ok(ICloudSessionState(Zeroizing::new(format!(
            "trust-for-{}",
            challenge.0.as_str()
        ))))
    }

    fn resume(
        &self,
        session: &ICloudSessionState,
    ) -> Result<(ICloudSessionState, ICloudHeaders), ICloudAuthError> {
        if *self.needs_reauth.lock().unwrap() {
            return Err(ICloudAuthError::NeedsReauthentication);
        }
        Ok((
            session.clone(),
            ICloudHeaders {
                headers: vec![(
                    "Cookie".into(),
                    Zeroizing::new("X-APPLE-WEBAUTH-TOKEN=short-lived".into()),
                )],
                expires_unix: Some(tilo_clock::unix_now().saturating_add(3600)),
            },
        ))
    }
}

#[test]
fn icloud_session_headers_are_handed_off_and_reauthentication_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tilo");
    let icloud = Arc::new(FakeICloud::default());
    let custodian = Custodian::for_root(Some(root.clone()))
        .with_icloud(Arc::clone(&icloud) as Arc<dyn ICloudAuth>);
    let ICloudSignIn::NeedsCode(challenge) = custodian
        .icloud()
        .sign_in("ada@icloud.com", "apple-password")
        .unwrap()
    else {
        panic!("expected a code step");
    };
    let session = custodian
        .icloud()
        .verify_code(&challenge, "123456")
        .unwrap();
    let id = AccountId::from("icloud-1");
    let secret = icloud_secret("ada@icloud.com", &session);
    save(
        &root,
        &id,
        ICLOUD_DRIVE_TYPE,
        &secret,
        serde_json::json!({}),
    );
    let sealed = SecretVault::open_in(&root).get(&id).unwrap();
    assert!(!format!("{sealed:?}").contains("apple-password"));
    assert_eq!(
        crate::icloud_apple_id(&sealed).as_deref(),
        Some("ada@icloud.com")
    );

    custodian.signed_in(&id).unwrap();
    match HandoffStore::open_in(&root).read(&id).unwrap() {
        Some(HandoffCredential::Session { headers, .. }) => {
            assert_eq!(headers.len(), 1);
            assert!(
                !headers[0].1.contains("trust-for"),
                "trust state stays sealed"
            );
        }
        other => panic!("unexpected {other:?}"),
    }

    *icloud.needs_reauth.lock().unwrap() = true;
    assert_eq!(
        custodian.publish(&id, true),
        Err(CustodyError::NeedsAuthentication)
    );
    assert_eq!(
        health(&root, &id),
        Some(AccountSyncHealth::NeedsAuthentication)
    );
}

#[test]
fn apple_sign_in_through_the_icloud_provider_hands_off_web_cookies() {
    use cloud_source_icloud::fake::{APPLE_ID, DEVICE_CODE, FakeICloud, PASSWORD};
    let fake = FakeICloud::start();
    let auth = crate::icloud::WebICloud::new(fake.endpoints()).unwrap();
    assert!(auth.is_available());
    let challenge = match auth.sign_in(APPLE_ID, PASSWORD).unwrap() {
        ICloudSignIn::NeedsCode(challenge) => challenge,
        ICloudSignIn::Complete(_) => panic!("the fake asks for a code"),
    };
    assert_eq!(
        auth.verify_code(&challenge, "000000").unwrap_err(),
        ICloudAuthError::InvalidCode
    );
    let session = auth.verify_code(&challenge, DEVICE_CODE).unwrap();
    assert_eq!(
        auth.storage_config(),
        serde_json::to_value(fake.config()).unwrap(),
        "accounts mount the drive this sign-in reached"
    );

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tilo");
    let id = AccountId::from("icloud-1");
    save(
        &root,
        &id,
        ICLOUD_DRIVE_TYPE,
        &icloud_secret(APPLE_ID, &session),
        serde_json::json!({ "identity": APPLE_ID }),
    );
    let custodian = Custodian::for_root(Some(root.clone())).with_icloud(Arc::new(auth));
    custodian.signed_in(&id).unwrap();
    let Some(HandoffCredential::Session { headers, .. }) =
        HandoffStore::open_in(&root).read(&id).unwrap()
    else {
        panic!("session headers");
    };
    assert!(
        headers
            .iter()
            .any(|(name, value)| name == "Cookie" && !value.is_empty())
    );

    // Apple wants the second factor again: the account needs sign-in.
    fake.expire_web_sessions();
    fake.expire_trust();
    assert_eq!(
        custodian.publish(&id, true),
        Err(CustodyError::NeedsAuthentication)
    );
}

#[test]
fn a_new_account_resolves_its_storage_once() {
    let graph = cloud_source_onedrive::fake::FakeGraph::personal();
    let custodian = Custodian::for_root(None).with_storage_config(
        OAuthCloud::OneDrive,
        serde_json::to_value(graph.config(cloud_source_onedrive::RootSelector::DriveRoot)).unwrap(),
    );
    let tokens = tilo_oauth::TokenSet {
        access_token: cloud_source_onedrive::fake::TOKEN.into(),
        ..tilo_oauth::TokenSet::default()
    };
    let mount: cloud_source_onedrive::Mount = serde_json::from_value(
        custodian
            .resolve_storage(OAuthCloud::OneDrive, &tokens)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(mount.drive_id, graph.drive_id());
    assert_eq!(mount.root, graph.drive_root());

    let dropbox = cloud_source_dropbox::fake::FakeDropbox::team();
    let mut config = cloud_source_dropbox::DropboxConfig::new("");
    config.endpoints = dropbox.endpoints();
    let custodian = Custodian::for_root(None)
        .with_storage_config(OAuthCloud::Dropbox, serde_json::to_value(config).unwrap());
    let tokens = tilo_oauth::TokenSet {
        access_token: cloud_source_dropbox::fake::TOKEN.into(),
        ..tilo_oauth::TokenSet::default()
    };
    let resolved: cloud_source_dropbox::DropboxConfig = serde_json::from_value(
        custodian
            .resolve_storage(OAuthCloud::Dropbox, &tokens)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        resolved.path_root.as_deref(),
        Some(cloud_source_dropbox::fake::TEAM_NAMESPACE),
        "a team member's calls are rooted in the team space"
    );

    let wrong = tilo_oauth::TokenSet {
        access_token: "not-accepted".into(),
        ..tilo_oauth::TokenSet::default()
    };
    assert_eq!(
        custodian.resolve_storage(OAuthCloud::Dropbox, &wrong),
        Err(CustodyError::NeedsAuthentication)
    );
}

const GMAIL: crate::OAuthUse = crate::OAuthUse::mail(crate::OAuthIssuer::Google);

#[test]
fn a_bare_token_grant_is_resealed_in_the_custody_format_on_first_use() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tilo");
    let id = AccountId::from("ada@gmail.com");
    let now = tilo_clock::unix_now();
    // The shape Mail sealed before its grants moved into custody.
    let bare = format!(
        r#"{{"access_token":"at-1","refresh_token":"rt-1","expires_in":3600,"obtained_at":{now}}}"#
    );
    let mut vault = SecretVault::open_in(&root);
    vault.put(
        &id,
        &Secret {
            password: String::new(),
            token: Some(bare),
        },
    );
    vault.try_save().unwrap();

    // Fresh, so no provider is asked (none is configured here).
    let custodian =
        Custodian::for_root(Some(root.clone())).with_client_table(crate::ClientTable::default());
    assert_eq!(
        *custodian.oauth_access_token(&id, GMAIL, false).unwrap(),
        "at-1"
    );
    let secret = SecretVault::open_in(&root).get(&id).unwrap();
    let grant: serde_json::Value = serde_json::from_str(secret.token.as_deref().unwrap()).unwrap();
    assert_eq!(grant["kind"], "oauth2");
    assert_eq!(grant["tokens"]["refresh_token"], "rt-1");
    assert_eq!(grant["tokens"]["obtained_at"], now);
}

#[test]
fn mail_and_drive_share_the_google_registration_with_their_own_scopes() {
    if option_env!("TILO_OAUTH_GOOGLE_CLIENT_ID").is_some()
        || option_env!("TILO_OAUTH_GOOGLE_CLIENT_SECRET").is_some()
    {
        return;
    }
    let table = crate::ClientTable::parse(
        r#"
        [google]
        client_id = "shared.apps.googleusercontent.com"
        client_secret = "desktop-secret"
        [google.scopes]
        files = ["drive"]
        mail = ["mail"]
        "#,
    )
    .unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_OAUTH_GOOGLE_CLIENT_ID")
        .remove("TILO_OAUTH_GOOGLE_CLIENT_SECRET")
        .remove("TILO_OAUTH_MICROSOFT_CLIENT_ID");
    let server = FakeAuthorizationServer::start();
    let custodian = Custodian::for_root(None)
        .with_client_table(table)
        .with_issuer_endpoints(crate::OAuthIssuer::Google, server.provider());
    let (mail, mail_client) = custodian.use_config(GMAIL).unwrap();
    let (drive, drive_client) = custodian
        .use_config(OAuthCloud::GoogleDrive.oauth_use())
        .unwrap();
    assert_eq!(mail.scopes, ["mail"]);
    assert_eq!(drive.scopes, ["drive"]);
    assert_eq!(mail.token_endpoint, format!("{}/token", server.url()));
    assert_eq!(mail_client, drive_client);
    assert_eq!(mail_client.secret(), Some("desktop-secret"));
    if option_env!("TILO_OAUTH_MICROSOFT_CLIENT_ID").is_none() {
        assert!(!custodian.is_configured(crate::OAuthUse::mail(crate::OAuthIssuer::Microsoft)));
    }
    assert!(matches!(
        custodian.begin_sign_in_for(crate::OAuthUse::mail(crate::OAuthIssuer::Dropbox), None),
        Err(CustodyError::NotConfigured {
            provider: "Dropbox mail",
            ..
        })
    ));
}

fn google_services_custodian(files: bool, mail: bool) -> Custodian {
    let mut table = crate::ClientTable::default();
    table.google.client_id = "google-services-client".into();
    if files {
        table.google.scopes.files = vec!["https://www.googleapis.com/auth/drive".into()];
    }
    if mail {
        table.google.scopes.mail = vec!["https://mail.google.com/".into()];
    }
    Custodian::for_root(None).with_client_table(table)
}

#[test]
fn google_services_sign_in_requests_only_selected_scopes() {
    let server = FakeAuthorizationServer::start();
    for (files, mail, expected) in [
        (true, false, "https://www.googleapis.com/auth/drive"),
        (false, true, "https://mail.google.com/"),
        (
            true,
            true,
            "https://www.googleapis.com/auth/drive https://mail.google.com/",
        ),
    ] {
        let custodian = google_services_custodian(files, mail)
            .with_issuer_endpoints(crate::OAuthIssuer::Google, server.provider());
        let authorization = custodian
            .begin_google_services_sign_in(files, mail)
            .unwrap();
        let url = url::Url::parse(authorization.url()).unwrap();
        let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["scope"], expected);
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(url.origin().ascii_serialization(), server.url());
        if files {
            let compatible = custodian.begin_google_sign_in(mail).unwrap();
            let url = url::Url::parse(compatible.url()).unwrap();
            let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
            assert_eq!(query["scope"], expected);
        }
    }
}

#[test]
fn google_services_sign_in_refuses_empty_or_unconfigured_selection() {
    assert!(matches!(
        google_services_custodian(true, true).begin_google_services_sign_in(false, false),
        Err(CustodyError::SignIn(_))
    ));
    for (files, mail) in [(true, false), (false, true)] {
        let custodian = google_services_custodian(files, mail);
        assert!(matches!(
            custodian.begin_google_services_sign_in(true, true),
            Err(CustodyError::NotConfigured { .. })
        ));
        assert!(matches!(
            custodian.begin_google_services_sign_in(!files, !mail),
            Err(CustodyError::NotConfigured { .. })
        ));
    }
}

#[test]
fn google_services_denied_permissions_are_rejected_before_api_requests() {
    // An unreachable origin proves a denied scope is rejected before contacting
    // the service, rather than relying on the service to reject the token.
    let custodian = google_services_custodian(true, true)
        .with_gmail_profile_endpoint("http://127.0.0.1:1/profile".into());
    for granted in ["", "https://www.googleapis.com/auth/drive"] {
        let tokens = tilo_oauth::TokenSet {
            scope: Some(granted.into()),
            ..Default::default()
        };
        assert_eq!(
            custodian.gmail_identity(&tokens),
            Err(CustodyError::SignIn(
                "Gmail permission was not granted".into()
            ))
        );
    }
    for granted in ["", "https://mail.google.com/"] {
        let tokens = tilo_oauth::TokenSet {
            scope: Some(granted.into()),
            ..Default::default()
        };
        assert_eq!(
            custodian.resolve_storage(OAuthCloud::GoogleDrive, &tokens),
            Err(CustodyError::SignIn(
                "Google Drive permission was not granted".into()
            ))
        );
    }
}

#[test]
fn scoped_sign_in_preserves_registration_and_refuses_extra_permissions() {
    let server = FakeAuthorizationServer::start();
    let mut table = crate::ClientTable::default();
    table.spotify.client_id = "registered-public-client".into();
    table.spotify.scopes.music = vec![
        "user-read-private".into(),
        "user-library-read".into(),
        "user-library-modify".into(),
    ];
    let use_ = crate::OAuthUse::new(crate::OAuthIssuer::Spotify, crate::Capability::Music);
    let custodian = Custodian::for_root(None)
        .with_client_table(table)
        .with_issuer_endpoints(crate::OAuthIssuer::Spotify, server.provider());
    for scopes in [
        &[][..],
        &["streaming"][..],
        &["user-library-read", "unconfigured"][..],
    ] {
        assert!(matches!(
            custodian.begin_sign_in_for_scopes(use_, scopes, None),
            Err(CustodyError::SignIn(_))
        ));
    }
    let authorization = custodian
        .begin_sign_in_for_scopes(use_, &["user-read-private", "user-library-read"], None)
        .unwrap();
    let url = url::Url::parse(authorization.url()).unwrap();
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["scope"], "user-read-private user-library-read");
    assert_eq!(query["client_id"], "registered-public-client");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(url.origin().ascii_serialization(), server.url());
}

#[test]
fn narrowed_scope_metadata_survives_a_refresh_that_omits_scope() {
    let (fixture, mut tokens) = signed_in_drive(3600);
    tokens.scope = Some("read-only".into());
    fixture.custodian.seal_oauth(&fixture.id, &tokens).unwrap();
    assert_eq!(
        fixture
            .custodian
            .oauth_scopes(&fixture.id)
            .unwrap()
            .as_deref(),
        Some("read-only")
    );
    fixture
        .custodian
        .oauth_access_token(&fixture.id, OAuthCloud::GoogleDrive.oauth_use(), true)
        .unwrap();
    assert_eq!(
        fixture
            .custodian
            .oauth_scopes(&fixture.id)
            .unwrap()
            .as_deref(),
        Some("read-only")
    );
}

#[test]
fn imported_cloud_clients_remain_per_grant_and_detach_never_revokes_source_sessions() {
    for cloud in [
        OAuthCloud::GoogleDrive,
        OAuthCloud::OneDrive,
        OAuthCloud::Dropbox,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("custody");
        let issuer = FakeAuthorizationServer::start();
        issuer.require_client_secret("original-public-client-secret");
        issuer.accept_refresh_token("original-refresh-token");
        let mut client = crate::Registration::public("original-public-client-id");
        client.client_secret = Some("original-public-client-secret".into());
        let registration =
            crate::ImportedRegistration::new(cloud, client, "consented-source".into()).unwrap();
        let tokens = tilo_oauth::TokenSet {
            refresh_token: "original-refresh-token".into(),
            ..Default::default()
        };
        let id = AccountId::new(format!("imported-{}", cloud.type_id()));
        save(
            &root,
            &id,
            cloud.type_id(),
            &crate::imported_oauth_secret(&tokens, &registration),
            serde_json::json!({}),
        );
        let custodian = Custodian::for_root(Some(root.clone())).with_provider(
            cloud,
            issuer.provider(),
            "different-current-client",
        );
        custodian
            .oauth_access_token(&id, cloud.oauth_use(), true)
            .unwrap();
        custodian
            .oauth_access_token(&id, cloud.oauth_use(), true)
            .unwrap();
        assert_eq!(issuer.refreshes(), 2);
        for request in issuer.requests() {
            let body = request.body_text();
            let form: std::collections::BTreeMap<_, _> =
                url::form_urlencoded::parse(body.as_bytes()).collect();
            assert_eq!(
                form.get("client_id").map(AsRef::as_ref),
                Some("original-public-client-id")
            );
            assert_eq!(
                form.get("client_secret").map(AsRef::as_ref),
                Some("original-public-client-secret")
            );
        }
        let secret = SecretVault::open_in(&root).get(&id).unwrap();
        assert!(crate::imported_oauth_matches(&secret, &registration));
        let account = AccountStore::load_in(&root).get(&id).cloned().unwrap();
        custodian.revoke(&account, &secret);
        assert!(issuer.revoked().is_empty());
        issuer.accept_refresh_token("original-refresh-token");
        assert!(
            custodian
                .refresh_imported_cloud(cloud, &tokens, &registration)
                .is_ok()
        );
    }
}

#[test]
fn copied_password_and_icloud_grants_preserve_credentials_and_detach_provenance() {
    let password = Secret::password("existing-nextcloud-app-password");
    let account = Account::new(AccountId::new("source-nextcloud"), "nextcloud");
    let copied = crate::copied_secret(&account, &password);
    assert!(copied.password.is_empty());
    assert_eq!(
        crate::password_credential(&copied).as_str(),
        password.password
    );
    assert!(crate::is_copied_secret(&copied));
    assert_eq!(crate::copied_secret(&account, &copied), copied);
    assert!(!crate::is_copied_secret(&password));
    let session = ICloudSessionState(Zeroizing::new("original-private-session".into()));
    let original = icloud_secret("ada@example.test", &session);
    let copied = crate::copied_secret(&account, &original);
    assert_eq!(
        crate::icloud_apple_id(&copied).as_deref(),
        Some("ada@example.test")
    );
    assert!(crate::is_copied_secret(&copied));
    assert_eq!(crate::copied_secret(&account, &copied), copied);
    assert!(!crate::is_copied_secret(&original));
}

#[test]
fn invalid_grant_clearing_cannot_satisfy_imported_or_copied_source_retry() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("custody");
    let cloud = OAuthCloud::GoogleDrive;
    let issuer = FakeAuthorizationServer::start();
    issuer.accept_refresh_token("source-refresh");
    let registration = crate::ImportedRegistration::new(
        cloud,
        crate::Registration::public("original-client"),
        "source-binding".into(),
    )
    .unwrap();
    let source_tokens = tilo_oauth::TokenSet {
        refresh_token: "source-refresh".into(),
        ..Default::default()
    };
    let custodian = Custodian::for_root(Some(root.clone())).with_provider(
        cloud,
        issuer.provider(),
        "tilo-client",
    );
    for (name, source) in [
        (
            "original-client",
            crate::imported_oauth_secret(&source_tokens, &registration),
        ),
        ("copied-tilo", oauth_secret(&source_tokens)),
    ] {
        let id = AccountId::new(name);
        let account = Account::new(id.clone(), cloud.type_id());
        let expected = crate::copied_secret(&account, &source);
        save(
            &root,
            &id,
            cloud.type_id(),
            &expected,
            serde_json::json!({}),
        );
        issuer.accept_refresh_token("source-refresh");
        custodian
            .oauth_access_token(&id, cloud.oauth_use(), true)
            .unwrap();
        assert!(crate::copied_source_matches(
            &SecretVault::open_in(&root).get(&id).unwrap(),
            &expected
        ));
        issuer.revoke_all_refresh_tokens();
        assert_eq!(
            custodian.oauth_access_token(&id, cloud.oauth_use(), true),
            Err(CustodyError::NeedsAuthentication)
        );
        let cleared = SecretVault::open_in(&root).get(&id).unwrap();
        assert!(crate::is_copied_secret(&cleared));
        assert!(!crate::copied_source_matches(&cleared, &expected));
        assert!(!crate::imported_oauth_matches(&cleared, &registration));
    }
}

#[test]
fn protected_copies_refuse_legacy_decoders_but_canonical_dav_custody_still_publishes() {
    #[derive(serde::Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum LegacyGrant {
        Oauth2 { tokens: crate::TokenSet },
        Icloud { apple_id: String, session: String },
    }
    let tokens = crate::TokenSet {
        access_token: "source-access".into(),
        refresh_token: "source-refresh".into(),
        expires_in: 3600,
        obtained_at: 0,
        scope: None,
    };
    let fresh = oauth_secret(&tokens);
    let LegacyGrant::Oauth2 { tokens: parsed } =
        serde_json::from_str::<LegacyGrant>(fresh.token.as_deref().unwrap()).unwrap()
    else {
        panic!("fresh OAuth changed its legacy wire kind")
    };
    assert_eq!(parsed.refresh_token, tokens.refresh_token);
    let session = ICloudSessionState(Zeroizing::new("source-session".into()));
    let fresh_icloud = icloud_secret("ada@example.test", &session);
    let LegacyGrant::Icloud {
        apple_id,
        session: parsed,
    } = serde_json::from_str::<LegacyGrant>(fresh_icloud.token.as_deref().unwrap()).unwrap()
    else {
        panic!("fresh iCloud changed its legacy wire kind")
    };
    assert_eq!(apple_id, "ada@example.test");
    assert_eq!(parsed, "source-session");
    let account = Account::new("source-cloud", GOOGLE_DRIVE_TYPE);
    let registration = crate::ImportedRegistration::new(
        OAuthCloud::GoogleDrive,
        tilo_oauth::Registration::public("original-client"),
        "source-binding".into(),
    )
    .unwrap();
    for protected in [
        crate::copied_secret(&account, &fresh),
        crate::imported_oauth_secret(&tokens, &registration),
        crate::copied_secret(&account, &fresh_icloud),
    ] {
        let raw = protected.token.as_deref().unwrap();
        assert!(serde_json::from_str::<LegacyGrant>(raw).is_err());
        assert!(serde_json::from_str::<crate::TokenSet>(raw).is_err());
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("target");
    for type_id in [WEBDAV_TYPE, crate::providers::NEXTCLOUD_TYPE] {
        let id = AccountId::new(type_id);
        let mut account = Account::new(id.clone(), type_id);
        account.config = serde_json::json!({"username":"ada", "app_password":true});
        let original = Secret::password("source-private-password");
        let copy = crate::copied_secret(&account, &original);
        assert!(copy.password.is_empty()); // Old Basic/revoke consumers refuse it.
        let raw = copy.token.as_deref().unwrap();
        assert!(serde_json::from_str::<LegacyGrant>(raw).is_err());
        assert!(serde_json::from_str::<crate::TokenSet>(raw).is_err());
        assert_eq!(
            crate::password_credential(&copy).as_str(),
            original.password
        );
        save(&root, &id, type_id, &copy, account.config.clone());
        Custodian::for_root(Some(root.clone()))
            .publish(&id, false)
            .unwrap();
        let Some(HandoffCredential::Basic { username, password }) =
            HandoffStore::open_in(&root).read(&id).unwrap()
        else {
            panic!("protected DAV grant was not consumed")
        };
        assert_eq!(username, "ada");
        assert_eq!(password.as_str(), original.password);
        assert_eq!(original, Secret::password("source-private-password"));
    }
    let ordinary = Secret::password("ordinary-mail-password");
    assert_eq!(
        crate::copied_secret(&Account::new("mail", "imap"), &ordinary).password,
        ordinary.password,
    );
}

#[test]
fn unknown_tag_cannot_fall_back_to_bare_oauth_tokens() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("target");
    let id = AccountId::new("future-grant");
    let secret = Secret {
        password: String::new(),
        token: Some(
            serde_json::json!({
                "kind":"future_protected_grant", "access_token":"private-access",
                "refresh_token":"private-refresh", "expires_in":3600,
            })
            .to_string(),
        ),
    };
    save(
        &root,
        &id,
        GOOGLE_DRIVE_TYPE,
        &secret,
        serde_json::json!({}),
    );
    assert!(matches!(
        Custodian::for_root(Some(root.clone())).publish(&id, false),
        Err(CustodyError::NeedsAuthentication),
    ));
    assert!(HandoffStore::open_in(&root).read(&id).unwrap().is_none());
    assert_eq!(SecretVault::open_in(&root).get(&id).unwrap(), secret);
}
/// Vertex AI signs in through Google's client with the cloud scope alone and
/// keeps its grant under its own id: a revoked or forgotten model grant
/// reads as "sign in again" or missing while Gmail's grant stays usable.
#[test]
fn a_model_grant_is_its_own_and_leaves_mail_alone() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tilo");
    let server = FakeAuthorizationServer::start();
    let mut table = crate::ClientTable::default();
    table.google.client_id = "shared.apps.googleusercontent.com".into();
    table.google.scopes.mail = vec!["mail".into()];
    table.google.scopes.models = vec!["cloud-platform".into()];
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_OAUTH_GOOGLE_CLIENT_ID")
        .remove("TILO_OAUTH_GOOGLE_CLIENT_SECRET");
    let custodian = Custodian::for_root(Some(root))
        .with_client_table(table)
        .with_issuer_endpoints(crate::OAuthIssuer::Google, server.provider());
    let models = crate::OAuthUse::new(crate::OAuthIssuer::Google, crate::Capability::Models);

    let authorization = custodian.begin_sign_in_for(models, None).unwrap();
    let url = url::Url::parse(authorization.url()).unwrap();
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["scope"], "cloud-platform");
    let link = authorization.url().to_string();
    let browser = std::thread::spawn(move || browse(&link));
    let tokens = Custodian::finish_sign_in(authorization).unwrap();
    browser.join().unwrap().unwrap();

    let grant = AccountId::from("agent-model-grant-p");
    let mail = AccountId::from("person@example.test");
    assert_eq!(
        custodian.oauth_grant_state(&grant),
        Ok(crate::GrantState::Missing)
    );
    custodian.seal_oauth(&grant, &tokens).unwrap();
    custodian.seal_oauth(&mail, &tokens).unwrap();
    assert_eq!(
        custodian.oauth_grant_state(&grant),
        Ok(crate::GrantState::Usable)
    );
    let (token, expires) = custodian
        .oauth_access_token_with_expiry(&grant, models, false)
        .unwrap();
    assert_eq!(*token, tokens.access_token);
    assert!(expires.is_some());

    server.revoke_all_refresh_tokens();
    assert_eq!(
        custodian.oauth_access_token(&grant, models, true),
        Err(CustodyError::NeedsAuthentication)
    );
    assert_eq!(
        custodian.oauth_grant_state(&grant),
        Ok(crate::GrantState::SignInAgain)
    );
    assert_eq!(
        custodian.oauth_grant_state(&mail),
        Ok(crate::GrantState::Usable)
    );
    custodian.forget_oauth(&grant).unwrap();
    assert_eq!(
        custodian.oauth_grant_state(&grant),
        Ok(crate::GrantState::Missing)
    );
    assert_eq!(
        custodian.oauth_grant_state(&mail),
        Ok(crate::GrantState::Usable)
    );
    // Forgetting is local: Google would revoke every grant of this client
    // for the person, Gmail's included.
    assert!(
        server
            .requests()
            .iter()
            .all(|request| !request.target.starts_with("/revoke"))
    );
}
