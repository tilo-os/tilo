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
        // A build or environment that provides the id starts a real flow.
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
