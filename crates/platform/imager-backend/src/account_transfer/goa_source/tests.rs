use super::super::Provider;
use super::*;
use accounts::AccountStore;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const CONFIG: &str = "[Account account_1]\nProvider=google\nIdentity=person@example.test\nPresentationIdentity=Personal files\nFilesEnabled=true\nOAuth2ClientId=original-goa-client\nOAuth2ClientSecret=original-goa-secret\n";
const TOKENS: &str = "{'access_token': <'old-source-access'>, 'refresh_token': <'source-goa-refresh'>, 'access_token_expires_at': <int64 1000000000000>, 'authorization_code': <'discard-code'>, 'password': <'discard-password'>}";

struct FixtureKeychain {
    calls: Cell<usize>,
    client: Option<&'static str>,
    change: Option<(PathBuf, String)>,
}
impl FixtureKeychain {
    fn original() -> Self {
        Self {
            calls: Cell::new(0),
            client: Some("original-goa-client"),
            change: None,
        }
    }
}
impl Keychain for FixtureKeychain {
    fn read(
        &self,
        definition: &Definition,
        directory: &std::fs::File,
    ) -> Result<GoaCredentials, ConnectionError> {
        self.calls.set(self.calls.get().saturating_add(1));
        assert_eq!(definition.id, "account_1");
        assert_eq!(definition.identity, "person@example.test");
        assert!(directory.metadata().unwrap().is_dir());
        if let Some((path, replacement)) = &self.change {
            std::fs::write(path, replacement).unwrap();
        }
        Ok(GoaCredentials {
            registration: self.client.map(|client| GoaRegistration {
                client_id: client.into(),
                client_secret: Zeroizing::new("original-goa-secret".into()),
            }),
            secret: tilo_secrets::Secret::new(TOKENS.as_bytes().to_vec()),
        })
    }
}
fn source(temporary: &Path, contents: &str) -> (SourceRoots, Candidate, PathBuf) {
    let config = temporary.join("source/.config");
    std::fs::create_dir_all(config.join("goa-1.0")).unwrap();
    let path = config.join("goa-1.0/accounts.conf");
    std::fs::write(&path, contents).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let roots = SourceRoots {
        home: temporary.join("source"),
        config,
    };
    let candidate = super::super::discover(&roots)
        .candidates
        .into_iter()
        .find(|candidate| candidate.origin == Origin::Gnome)
        .unwrap();
    (roots, candidate, path)
}

fn api_server(
    replies: Vec<(&'static str, serde_json::Value)>,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        for (expected, reply) in replies {
            let deadline = std::time::Instant::now()
                .checked_add(std::time::Duration::from_secs(15))
                .unwrap();
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "expected production API request did not arrive"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("fixture listener failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(15)))
                .unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
                assert!(bytes.len() < 16 * 1024);
            }
            let request = String::from_utf8(bytes).unwrap();
            assert!(request.starts_with(&format!("GET {expected} HTTP/1.1\r\n")));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer ")
            );
            assert!(!request.contains("old-source-access"));
            let body = serde_json::to_vec(&reply).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    (base, worker)
}
fn connector(target: &Path, issuer: &tilo_oauth::fake::FakeAuthorizationServer) -> Connector {
    Connector::with_custodian(
        target.to_owned(),
        account_custody::Custodian::for_root(Some(target.to_owned())).with_provider(
            OAuthCloud::GoogleDrive,
            issuer.provider(),
            "different-tilo-client",
        ),
    )
}

fn assert_original_requests(issuer: &tilo_oauth::fake::FakeAuthorizationServer) {
    for request in issuer
        .requests()
        .iter()
        .filter(|request| request.target.starts_with("/token"))
    {
        let body = request.body_text();
        let form = url::form_urlencoded::parse(body.as_bytes()).collect::<BTreeMap<_, _>>();
        assert_eq!(
            form.get("client_id").map(|value| value.as_ref()),
            Some("original-goa-client")
        );
        assert_eq!(
            form.get("client_secret").map(|value| value.as_ref()),
            Some("original-goa-secret")
        );
    }
}
fn reseal_refresh_and_remove(
    temporary: &Path,
    source: &Connector,
    id: &AccountId,
    issuer: &tilo_oauth::fake::FakeAuthorizationServer,
) {
    let key = tilo_seal::SymmetricKey::generate().unwrap();
    let sealed = source
        .seal_selected_accounts(std::slice::from_ref(id), &key, b"goa-portable-transport")
        .unwrap();
    let root = temporary.join("restored");
    let restored = connector(&root, issuer);
    restored
        .import_sealed_accounts(&sealed, &key, b"goa-portable-transport")
        .unwrap();
    restored
        .custodian
        .oauth_access_token(id, OAuthCloud::GoogleDrive.oauth_use(), true)
        .unwrap();
    assert_eq!(issuer.refreshes(), 2);
    restored.disconnect(id).unwrap();
    assert!(AccountStore::load_in(&root).get(id).is_none());
    assert!(SecretVault::open_read_only_in(&root).get(id).is_none());
    assert!(issuer.revoked().is_empty());
    assert!(AccountStore::load_in(&source.root).get(id).is_some());
}

#[test]
fn goa_original_registration_refreshes_verifies_identity_and_root_and_retries_without_source_writes()
 {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temporary = tempfile::tempdir().unwrap();
    let (roots, selected, path) = source(temporary.path(), CONFIG);
    assert!(selected.can_import_credentials);
    let projection = serde_json::to_string(&super::super::discover(&roots)).unwrap();
    for secret in [
        "original-goa-secret",
        "source-goa-refresh",
        "old-source-access",
        "discard-password",
    ] {
        assert!(!projection.contains(secret));
    }
    let before = std::fs::metadata(&path).unwrap();
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    issuer.require_client_secret("original-goa-secret");
    issuer.accept_refresh_token("source-goa-refresh");
    let (api, worker) = api_server(vec![
        (
            "/oauth2/v2/userinfo",
            serde_json::json!({"email":"person@example.test"}),
        ),
        (
            "/drive/v3/files/root",
            serde_json::json!({"id":"actual-drive-root"}),
        ),
    ]);
    // The guided account stage has a home but no destination .config yet.
    let stage = temporary.path().join("guided-stage");
    std::fs::create_dir(&stage).unwrap();
    std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = stage.join(".config/tilo");
    assert!(!target.parent().unwrap().exists());
    let mut connector = connector(&target, &issuer);
    connector.goa_api = Some(api);
    let keychain = FixtureKeychain::original();
    let id = connector
        .import_goa_with(&roots, &selected, &keychain)
        .unwrap();
    worker.join().unwrap();
    assert_eq!(issuer.refreshes(), 1);
    assert_eq!(
        connector
            .import_goa_with(&roots, &selected, &keychain)
            .unwrap(),
        id
    );
    assert_eq!(issuer.refreshes(), 1);
    assert_eq!(keychain.calls.get(), 2);
    let store = AccountStore::load_in(&target);
    let account = store.get(&id).unwrap();
    assert_eq!(account.type_id.as_str(), OAuthCloud::GoogleDrive.type_id());
    assert_eq!(account.config["source_goa_identity"], "person@example.test");
    assert!(
        account
            .config
            .get(accounts::cloud_types::MOUNT_CONFIG_KEY)
            .is_some()
    );
    let public = std::fs::read_to_string(accounts::store_path_in(&target)).unwrap();
    assert!(!public.contains("original-goa-secret"));
    assert!(!public.contains("source-goa-refresh"));
    let private = SecretVault::open_read_only_in(&target).get(&id).unwrap();
    let credentials = keychain
        .read(
            &definitions(CONFIG, true).remove(0),
            &std::fs::File::open(&roots.config).unwrap(),
        )
        .unwrap();
    let tokens =
        super::super::goa_variant::parse(&Zeroizing::new(credentials.secret.expose().to_vec()))
            .unwrap();
    let original =
        original_registration(&definitions(CONFIG, true).remove(0), &credentials, &tokens).unwrap();
    assert!(account_custody::imported_oauth_matches(&private, &original));
    reseal_refresh_and_remove(temporary.path(), &connector, &id, &issuer);
    assert_original_requests(&issuer);
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG);
    assert_eq!(after.permissions().mode(), before.permissions().mode());
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
}

#[test]
fn goa_wrong_and_changed_reviewed_source_refuse_before_keychain_or_destination() {
    let temporary = tempfile::tempdir().unwrap();
    let (roots, mut selected, path) = source(temporary.path(), CONFIG);
    let target = temporary.path().join("target");
    let connector = Connector::new(target.clone());
    let keychain = FixtureKeychain::original();
    let original_path = selected.profile.clone();
    selected.profile = temporary.path().join("other/accounts.conf");
    assert_eq!(
        connector.import_goa_with(&roots, &selected, &keychain),
        Err(ConnectionError::InvalidConfiguration)
    );
    selected.profile = original_path;
    std::fs::write(
        &path,
        CONFIG.replace("person@example.test", "different@example.test"),
    )
    .unwrap();
    assert_eq!(
        connector.import_goa_with(&roots, &selected, &keychain),
        Err(ConnectionError::AccountMissing)
    );
    assert_eq!(keychain.calls.get(), 0);
    assert!(!target.exists());
}

#[test]
fn goa_source_change_during_keychain_read_refuses_before_refresh() {
    let temporary = tempfile::tempdir().unwrap();
    let (roots, selected, path) = source(temporary.path(), CONFIG);
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    issuer.accept_refresh_token("source-goa-refresh");
    let target = temporary.path().join("target");
    let connector = connector(&target, &issuer);
    let mut keychain = FixtureKeychain::original();
    keychain.change = Some((path, CONFIG.replace("Personal files", "Changed files")));
    assert_eq!(
        connector.import_goa_with(&roots, &selected, &keychain),
        Err(ConnectionError::AccountMissing)
    );
    assert_eq!(keychain.calls.get(), 1);
    assert_eq!(issuer.refreshes(), 0);
    assert!(!target.exists());
}

#[test]
fn goa_missing_or_mismatched_runtime_registration_has_no_fabricated_fallback() {
    let temporary = tempfile::tempdir().unwrap();
    let (roots, selected, _) = source(temporary.path(), CONFIG);
    let target = temporary.path().join("target");
    let connector = Connector::new(target.clone());
    let mut keychain = FixtureKeychain::original();
    keychain.client = Some("different-runtime-client");
    assert_eq!(
        connector.import_goa_with(&roots, &selected, &keychain),
        Err(ConnectionError::AccountMissing)
    );
    assert!(!target.exists());
    let without_override = CONFIG
        .lines()
        .filter(|line| !line.starts_with("OAuth2Client"))
        .collect::<Vec<_>>()
        .join("\n");
    let (roots, selected, _) = source(temporary.path(), &without_override);
    keychain.client = None;
    assert_eq!(
        connector.import_goa_with(&roots, &selected, &keychain),
        Err(ConnectionError::CredentialUnavailable)
    );
    assert!(!target.exists());
}

#[test]
fn goa_refreshed_remote_identity_mismatch_prevents_persistence() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temporary = tempfile::tempdir().unwrap();
    let (roots, selected, path) = source(temporary.path(), CONFIG);
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    issuer.require_client_secret("original-goa-secret");
    issuer.accept_refresh_token("source-goa-refresh");
    let (api, worker) = api_server(vec![(
        "/oauth2/v2/userinfo",
        serde_json::json!({"email":"another@example.test"}),
    )]);
    let target = temporary.path().join("target");
    let mut connector = connector(&target, &issuer);
    connector.goa_api = Some(api);
    assert_eq!(
        connector.import_goa_with(&roots, &selected, &FixtureKeychain::original()),
        Err(ConnectionError::AccountMissing)
    );
    worker.join().unwrap();
    assert_eq!(issuer.refreshes(), 1);
    assert!(!accounts::store_path_in(&target).exists());
    assert_eq!(std::fs::read_to_string(path).unwrap(), CONFIG);
}

#[test]
fn goa_identity_alone_does_not_prove_access_to_files() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temporary = tempfile::tempdir().unwrap();
    let (roots, selected, path) = source(temporary.path(), CONFIG);
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    issuer.require_client_secret("original-goa-secret");
    issuer.accept_refresh_token("source-goa-refresh");
    let (api, worker) = api_server(vec![
        (
            "/oauth2/v2/userinfo",
            serde_json::json!({"email":"person@example.test"}),
        ),
        ("/drive/v3/files/root", serde_json::json!({})),
    ]);
    let target = temporary.path().join("target");
    let mut connector = connector(&target, &issuer);
    connector.goa_api = Some(api);
    assert_eq!(
        connector.import_goa_with(&roots, &selected, &FixtureKeychain::original()),
        Err(ConnectionError::CredentialUnavailable)
    );
    worker.join().unwrap();
    assert_eq!(issuer.refreshes(), 1);
    assert!(!accounts::store_path_in(&target).exists());
    assert_eq!(std::fs::read_to_string(path).unwrap(), CONFIG);
}

#[test]
fn goa_runtime_defaults_and_explicit_config_fallback_preserve_actual_original_registration() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let without_overrides = CONFIG
        .lines()
        .filter(|line| !line.starts_with("OAuth2Client"))
        .collect::<Vec<_>>()
        .join("\n");
    for (contents, runtime_registration) in [(without_overrides.as_str(), true), (CONFIG, false)] {
        let temporary = tempfile::tempdir().unwrap();
        let (roots, selected, path) = source(temporary.path(), contents);
        let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
        issuer.require_client_secret("original-goa-secret");
        issuer.accept_refresh_token("source-goa-refresh");
        let (api, worker) = api_server(vec![
            (
                "/oauth2/v2/userinfo",
                serde_json::json!({"email":"person@example.test"}),
            ),
            (
                "/drive/v3/files/root",
                serde_json::json!({"id":"actual-root"}),
            ),
        ]);
        let target = temporary.path().join("target");
        let mut connector = connector(&target, &issuer);
        connector.goa_api = Some(api);
        let mut keychain = FixtureKeychain::original();
        if !runtime_registration {
            keychain.client = None;
        }
        let id = connector
            .import_goa_with(&roots, &selected, &keychain)
            .unwrap();
        worker.join().unwrap();
        assert_eq!(issuer.refreshes(), 1);
        assert_original_requests(&issuer);
        assert!(SecretVault::open_read_only_in(&target).get(&id).is_some());
        assert_eq!(std::fs::read_to_string(path).unwrap(), contents);
    }
}

#[test]
fn goa_onedrive_uses_original_runtime_grant_and_canonical_storage_consumer() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temporary = tempfile::tempdir().unwrap();
    let contents = CONFIG.replace("Provider=google", "Provider=ms_graph");
    let (roots, selected, path) = source(temporary.path(), &contents);
    assert_eq!(selected.provider, Provider::Microsoft);
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    issuer.require_client_secret("original-goa-secret");
    issuer.accept_refresh_token("source-goa-refresh");
    let (api, worker) = api_server(vec![
        (
            "/v1.0/me",
            serde_json::json!({"mail":"person@example.test"}),
        ),
        (
            "/graph/me/drive?$select=id,driveType",
            serde_json::json!({"id":"selected-drive","driveType":"personal"}),
        ),
        (
            "/graph/drives/selected-drive/root?$select=id",
            serde_json::json!({"id":"selected-root"}),
        ),
    ]);
    let target = temporary.path().join("target");
    let custodian = account_custody::Custodian::for_root(Some(target.clone()))
        .with_provider(OAuthCloud::OneDrive, issuer.provider(), "different-tilo-client")
        .with_storage_config(OAuthCloud::OneDrive, serde_json::json!({
            "api":format!("{api}/graph/"), "drive":"Me", "root":"DriveRoot", "upload_chunk":10485760
        }));
    let mut connector = Connector::with_custodian(target.clone(), custodian);
    connector.goa_api = Some(api);
    let id = connector
        .import_goa_with(&roots, &selected, &FixtureKeychain::original())
        .unwrap();
    worker.join().unwrap();
    assert_eq!(issuer.refreshes(), 1);
    assert_original_requests(&issuer);
    let store = AccountStore::load_in(&target);
    let account = store.get(&id).unwrap();
    assert_eq!(account.type_id.as_str(), OAuthCloud::OneDrive.type_id());
    let mount = &account.config[accounts::cloud_types::MOUNT_CONFIG_KEY];
    assert_eq!(mount["drive_id"], "selected-drive");
    assert_eq!(mount["drive_root"], "selected-root");
    assert_eq!(mount["root"], "selected-root");
    assert!(SecretVault::open_read_only_in(&target).get(&id).is_some());
    assert_eq!(std::fs::read_to_string(path).unwrap(), contents);
}

#[test]
fn goa_discovery_preserves_distinct_accounts_and_refuses_unrecognized_issuer_overrides() {
    let temporary = tempfile::tempdir().unwrap();
    let contents = format!(
        "{CONFIG}\n{}",
        CONFIG
            .replace("account_1", "account_2")
            .replace("person@example.test", "other@example.test")
    );
    let (roots, _, _) = source(temporary.path(), &contents);
    let inventory = super::super::discover(&roots);
    assert_eq!(inventory.candidates.len(), 2);
    assert_ne!(inventory.candidates[0].id, inventory.candidates[1].id);
    assert!(
        inventory
            .candidates
            .iter()
            .all(|candidate| candidate.can_import_credentials)
    );
    let (_, selected, _) = source(
        temporary.path(),
        &format!("{CONFIG}OAuth2TokenUri=https://foreign.example.test/token\n"),
    );
    assert!(!selected.can_import_credentials);
    let keychain = FixtureKeychain::original();
    let connector = Connector::new(temporary.path().join("target"));
    assert_eq!(
        connector.import_goa_with(&roots, &selected, &keychain),
        Err(ConnectionError::InvalidConfiguration)
    );
    assert_eq!(keychain.calls.get(), 0);
}

#[test]
fn goa_mail_only_or_disabled_files_remain_connectable_metadata_without_extraction() {
    let temporary = tempfile::tempdir().unwrap();
    for config in [
        CONFIG.replace("FilesEnabled=true", "FilesEnabled=false"),
        CONFIG.replace("FilesEnabled=true\n", ""),
    ] {
        let (roots, selected, _) = source(temporary.path(), &config);
        assert_eq!(selected.provider, Provider::Google);
        assert!(!selected.can_import_credentials);
        let keychain = FixtureKeychain::original();
        assert_eq!(
            Connector::new(temporary.path().join("target"))
                .import_goa_with(&roots, &selected, &keychain),
            Err(ConnectionError::InvalidConfiguration)
        );
        assert_eq!(keychain.calls.get(), 0);
        assert!(!temporary.path().join("target").exists());
    }
}

#[derive(serde::Deserialize)]
struct StoredExpiry {
    tokens: TokenExpiry,
}
#[derive(serde::Deserialize)]
struct TokenExpiry {
    obtained_at: u64,
    expires_in: u64,
}
fn wait_for_actual_expiry(root: &Path, id: &AccountId) {
    let private =
        super::super::connect::PrivateSecret(SecretVault::open_read_only_in(root).get(id).unwrap());
    let stored: StoredExpiry = serde_json::from_str(private.0.token.as_deref().unwrap()).unwrap();
    assert_eq!(stored.tokens.expires_in, 1);
    let expiry = stored
        .tokens
        .obtained_at
        .checked_add(stored.tokens.expires_in)
        .unwrap();
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(5))
        .unwrap();
    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if now >= expiry {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "synthetic grant did not reach its actual expiry"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn expired_imported_goa_grant_renews_automatically_after_reseal_under_original_client_without_source_revocation()
 {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temporary = tempfile::tempdir().unwrap();
    let (roots, selected, path) = source(temporary.path(), CONFIG);
    let before = std::fs::metadata(&path).unwrap();
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    issuer.require_client_secret("original-goa-secret");
    issuer.accept_refresh_token("source-goa-refresh");
    issuer.set_token_lifetime(1);
    issuer.issue_access_token("expired-imported-access");
    let (api, worker) = api_server(vec![
        (
            "/oauth2/v2/userinfo",
            serde_json::json!({"email":"person@example.test"}),
        ),
        (
            "/drive/v3/files/root",
            serde_json::json!({"id":"actual-drive-root"}),
        ),
    ]);
    let original = temporary.path().join("prepared");
    let mut imported = connector(&original, &issuer);
    imported.goa_api = Some(api);
    let id = imported
        .import_goa_with(&roots, &selected, &FixtureKeychain::original())
        .unwrap();
    worker.join().unwrap();
    assert_eq!(issuer.refreshes(), 1);
    let key = tilo_seal::SymmetricKey::generate().unwrap();
    let sealed = imported
        .seal_selected_accounts(std::slice::from_ref(&id), &key, b"expired-goa-renewal")
        .unwrap();
    let target = temporary.path().join("restored");
    let restored = connector(&target, &issuer);
    restored
        .import_sealed_accounts(&sealed, &key, b"expired-goa-renewal")
        .unwrap();
    wait_for_actual_expiry(&target, &id);
    issuer.set_token_lifetime(3600);
    issuer.issue_access_token("automatically-renewed-access");
    let access = restored
        .custodian
        .oauth_access_token(&id, OAuthCloud::GoogleDrive.oauth_use(), false)
        .unwrap();
    assert_eq!(access.as_str(), "automatically-renewed-access");
    assert_eq!(issuer.refreshes(), 2);
    let cached = restored
        .custodian
        .oauth_access_token(&id, OAuthCloud::GoogleDrive.oauth_use(), false)
        .unwrap();
    assert_eq!(cached.as_str(), access.as_str());
    assert_eq!(issuer.refreshes(), 2);
    assert_original_requests(&issuer);
    restored.disconnect(&id).unwrap();
    assert!(issuer.revoked().is_empty());
    assert!(AccountStore::load_in(&original).get(&id).is_some());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG);
    let after = std::fs::metadata(path).unwrap();
    assert_eq!(after.permissions().mode(), before.permissions().mode());
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
}
