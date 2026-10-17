use super::*;
use accounts::{Account, AccountId, AccountStore, Secret, SecretVault};

#[test]
fn consented_sources_extract_multiple_real_accounts_without_exposing_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("goa-1.0")).unwrap();
    std::fs::create_dir_all(config.join("Nextcloud")).unwrap();
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    std::fs::write(config.join("goa-1.0/accounts.conf"), "[Account first]\nProvider=google\nPresentationIdentity=ada@gmail.example\npassword=DO_NOT_LEAK\n[Account second]\nProvider=ms_graph\nPresentationIdentity=work@example.test\n").unwrap();
    std::fs::write(config.join("Nextcloud/nextcloud.cfg"), "[Accounts]\n0\\url=https://cloud.example.test/base\n0\\webflow_user=ada\n0\\password=DO_NOT_LEAK\n1\\url=https://ada:DO_NOT_LEAK@bad.example\n1\\user=ada\n").unwrap();
    std::fs::write(config.join("rclone/rclone.conf"), "[My drive]\ntype=drive\ntoken={\"refresh_token\":\"DO_NOT_LEAK\"}\n[Team files]\ntype=webdav\nvendor=nextcloud\nurl=https://team.example.test/\nuser=ada\npass=DO_NOT_LEAK\n").unwrap();
    std::fs::create_dir_all(config.join("Slack/Local Storage/leveldb")).unwrap();
    std::fs::create_dir_all(home.join(".var/app/com.discordapp.Discord/config/discord")).unwrap();
    std::fs::create_dir_all(home.join(".mozilla/firefox/fixture.default")).unwrap();
    let inventory = discover(&SourceRoots { home, config });
    assert!(inventory.unreadable_sources.is_empty());
    assert_eq!(inventory.candidates.len(), 8);
    assert_eq!(
        inventory
            .candidates
            .iter()
            .filter(|c| c.provider == Provider::Nextcloud)
            .count(),
        2
    );
    assert!(inventory.candidates.iter().all(|c| c.needs_sign_in));
    assert!(
        !serde_json::to_string(&inventory)
            .unwrap()
            .contains("DO_NOT_LEAK")
    );
}

#[cfg(unix)]
#[test]
fn discovery_refuses_symlink_escape_and_oversize_configuration() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        outside.join("accounts.conf"),
        "[Account a]\nProvider=google\nIdentity=outside\n",
    )
    .unwrap();
    symlink(&outside, config.join("goa-1.0")).unwrap();
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    std::fs::write(
        config.join("rclone/rclone.conf"),
        vec![b'a'; 2 * 1024 * 1024 + 1],
    )
    .unwrap();
    let inventory = discover(&SourceRoots {
        home: temp.path().join("home"),
        config,
    });
    assert!(inventory.candidates.is_empty());
    assert_eq!(inventory.unreadable_sources.len(), 2);
}

#[test]
fn webdav_connection_is_sealed_and_readable_by_existing_account_consumers() {
    let dav_server = crate::account_transfer::dav_readiness::test_server();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("target");
    let connector = Connector::new(root.clone());
    let id = connector
        .connect_webdav(
            "Files",
            &format!("{}/dav", dav_server.url()),
            "ada",
            zeroize::Zeroizing::new("PRIVATE_APP_PASSWORD".into()),
        )
        .unwrap();
    let store = AccountStore::load_in(&root);
    let account = store.get(&id).unwrap();
    assert_eq!(account.type_id.as_str(), "webdav");
    assert_eq!(account.config["username"], "ada");
    let metadata = std::fs::read_to_string(accounts::store_path_in(&root)).unwrap();
    assert!(!metadata.contains("PRIVATE_APP_PASSWORD"));
    let vault = SecretVault::open_in(&root);
    assert_eq!(
        account_custody::password_credential(&vault.get(&id).unwrap()).to_string(),
        "PRIVATE_APP_PASSWORD"
    );
    let encrypted = std::fs::read(accounts::vault_dir_in(&root).join("vault.enc")).unwrap();
    assert!(!String::from_utf8_lossy(&encrypted).contains("PRIVATE_APP_PASSWORD"));
    for url in [
        "http://untrusted.example/dav",
        "https://ada:password@files.example/dav",
        "https://files.example/dav?token=secret",
    ] {
        assert_eq!(
            connector.connect_webdav(
                "Files",
                url,
                "ada",
                zeroize::Zeroizing::new("password".into())
            ),
            Err(ConnectionError::InvalidConfiguration)
        );
    }
}

#[test]
fn selected_tilo_credentials_are_resealed_without_overwriting_or_adopting_collaborators() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    let own = AccountId::new("own");
    let other = AccountId::new("other");
    let mut store = AccountStore::new();
    store.upsert(Account::new(own.clone(), "google-drive"));
    let mut shared = Account::new(other.clone(), "dropbox");
    shared.owner = Some("another-person".into());
    store.upsert(shared);
    store.save_in(&source).unwrap();
    let mut vault = SecretVault::open_in(&source);
    vault.put(
        &own,
        &Secret {
            password: String::new(),
            token: Some("PRIVATE_REFRESH_GRANT".into()),
        },
    );
    vault.try_save().unwrap();
    let original = std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    let connector = Connector::new(target.clone());
    assert_eq!(connector.import_tilo(&source, &own).unwrap(), own);
    assert_eq!(
        SecretVault::open_in(&target)
            .get(&own)
            .unwrap()
            .token
            .as_deref(),
        Some("PRIVATE_REFRESH_GRANT")
    );
    assert_eq!(connector.import_tilo(&source, &own).unwrap(), own);
    assert_eq!(
        connector.import_tilo(&source, &other),
        Err(ConnectionError::AccountMissing)
    );
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        original
    );
}

#[test]
fn selected_rclone_app_password_is_extracted_and_invalid_oauth_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("source/.config");
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    let source = config.join("rclone/rclone.conf");
    let original = "[Team]\ntype=webdav\nvendor=nextcloud\nurl=https://cloud.example.test/remote.php/dav/files/ada/\nuser=ada\npass=AAECAwQFBgcICQoLDA0OD2f2H1FyoCWnrFF4SHSBvFFHjlw9\n[Drive]\ntype=drive\ntoken=FOREIGN_GRANT\n";
    std::fs::write(&source, original).unwrap();
    let roots = SourceRoots {
        home: temp.path().join("source"),
        config,
    };
    let inventory = discover(&roots);
    let team = inventory
        .candidates
        .iter()
        .find(|c| c.label == "Team")
        .unwrap();
    assert!(team.can_import_credentials);
    let target = temp.path().join("target");
    let connector = Connector::new(target.clone());
    let id = connector.import_rclone(&roots, team).unwrap();
    let account = AccountStore::load_in(&target).get(&id).cloned().unwrap();
    assert_eq!(account.type_id.as_str(), "webdav");
    assert_eq!(
        account.config["server_url"],
        "https://cloud.example.test/remote.php/dav/files/ada"
    );
    assert_eq!(
        account_custody::password_credential(&SecretVault::open_in(&target).get(&id).unwrap())
            .to_string(),
        "fixture-app-password"
    );
    assert_eq!(connector.import_rclone(&roots, team).unwrap(), id);
    let drive = inventory
        .candidates
        .iter()
        .find(|c| c.label == "Drive")
        .unwrap();
    assert_eq!(
        connector.import_rclone(&roots, drive),
        Err(ConnectionError::CredentialUnavailable)
    );
    assert_eq!(std::fs::read_to_string(&source).unwrap(), original);
    let mut changed = team.clone();
    changed.server = Some("https://different.example/".into());
    assert_eq!(
        connector.import_rclone(&roots, &changed),
        Err(ConnectionError::AccountMissing)
    );
}

#[test]
fn source_native_tilo_spotify_grants_remain_usable_in_both_canonical_music_slots() {
    use account_custody::{Capability, Custodian, OAuthIssuer, OAuthUse, TokenSet};
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let target = root.path().join("target");
    let mut store = AccountStore::new();
    let source_custody = Custodian::for_root(Some(source.clone()));
    for (slot, scope, token) in [
        (
            "music.spotify",
            "user-read-private user-library-read",
            "private-source-catalogue-access",
        ),
        (
            "music.spotify.streaming",
            "streaming user-read-private",
            "private-source-streaming-access",
        ),
    ] {
        let id = AccountId::new(slot);
        let mut account = Account::new(id.clone(), "spotify");
        account.display_name = "Spotify".into();
        account.config = serde_json::json!({"provider":"spotify"});
        store.upsert(account);
        store.save_in(&source).unwrap();
        source_custody
            .seal_oauth(
                &id,
                &TokenSet {
                    access_token: token.into(),
                    refresh_token: format!("private-refresh-{slot}"),
                    expires_in: 3600,
                    scope: Some(scope.into()),
                    ..Default::default()
                }
                .stamp_now(),
            )
            .unwrap();
    }
    let source_vault = accounts::vault_dir_in(&source).join("vault.enc");
    let original_vault = std::fs::read(&source_vault).unwrap();
    let original_store = std::fs::read(accounts::store_path_in(&source)).unwrap();
    let connector = Connector::new(target.clone());
    let target_custody = Custodian::for_root(Some(target.clone()));
    for (slot, capability, token) in [
        (
            "music.spotify",
            Capability::Music,
            "private-source-catalogue-access",
        ),
        (
            "music.spotify.streaming",
            Capability::MusicStreaming,
            "private-source-streaming-access",
        ),
    ] {
        let id = AccountId::new(slot);
        assert_eq!(connector.import_tilo(&source, &id).unwrap(), id);
        let usable = target_custody
            .oauth_access_token(&id, OAuthUse::new(OAuthIssuer::Spotify, capability), false)
            .unwrap();
        assert_eq!(usable.as_str(), token);
        assert_eq!(connector.import_tilo(&source, &id).unwrap(), id);
    }
    assert_eq!(std::fs::read(source_vault).unwrap(), original_vault);
    assert_eq!(
        std::fs::read(accounts::store_path_in(&source)).unwrap(),
        original_store
    );
    let target_metadata = std::fs::read_to_string(accounts::store_path_in(&target)).unwrap();
    assert!(!target_metadata.contains("private-source-"));
    assert!(!target_metadata.contains("private-refresh-"));
}

#[test]
fn cloud_connection_uses_real_pkce_exchange_and_canonical_sealed_grant() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("target");
    let server = tilo_oauth::fake::FakeAuthorizationServer::start();
    let custodian = account_custody::Custodian::for_root(Some(root.clone())).with_provider(
        account_custody::OAuthCloud::GoogleDrive,
        server.provider(),
        "tilo-client",
    );
    let connector = Connector::with_custodian(root.clone(), custodian);
    let authorization = connector
        .begin_oauth(
            account_custody::OAuthCloud::GoogleDrive,
            Some("ada@example.test"),
        )
        .unwrap();
    let url = authorization.url().to_string();
    let browser = std::thread::spawn(move || tilo_oauth::fake::browse(&url));
    let id = connector
        .finish_oauth(
            account_custody::OAuthCloud::GoogleDrive,
            "My Drive",
            authorization,
        )
        .unwrap();
    browser.join().unwrap().unwrap();
    let store = AccountStore::load_in(&root);
    let account = store.get(&id).unwrap();
    assert_eq!(account.type_id.as_str(), "google-drive");
    assert!(
        account
            .config
            .get(accounts::cloud_types::MOUNT_CONFIG_KEY)
            .is_some()
    );
    let secret = SecretVault::open_in(&root).get(&id).unwrap();
    assert!(secret.token.unwrap().contains("refresh_token"));
    assert!(
        !std::fs::read_to_string(accounts::store_path_in(&root))
            .unwrap()
            .contains("refresh_token")
    );
}

#[test]
fn retry_does_not_overwrite_an_unrelated_target_credential() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    let id = AccountId::new("selected");
    let mut store = AccountStore::new();
    store.upsert(Account::new(id.clone(), "webdav"));
    store.save_in(&source).unwrap();
    store.save_in(&target).unwrap();
    let mut source_vault = SecretVault::open_in(&source);
    source_vault.put(&id, &Secret::password("source-secret"));
    source_vault.try_save().unwrap();
    let mut target_vault = SecretVault::open_in(&target);
    target_vault.put(&id, &Secret::password("different-target-secret"));
    target_vault.try_save().unwrap();
    let before = std::fs::read(accounts::vault_dir_in(&target).join("vault.enc")).unwrap();
    assert_eq!(
        Connector::new(target.clone()).import_tilo(&source, &id),
        Err(ConnectionError::AlreadyPresent)
    );
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&target).join("vault.enc")).unwrap(),
        before
    );
}

#[test]
fn connecting_refuses_a_corrupt_existing_account_store_without_replacing_it() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("target");
    let path = accounts::store_path_in(&root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "broken-existing-accounts").unwrap();
    assert_eq!(
        Connector::new(root).connect_webdav(
            "Files",
            "https://files.example.test",
            "ada",
            zeroize::Zeroizing::new("password".into())
        ),
        Err(ConnectionError::StorageUnavailable)
    );
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "broken-existing-accounts"
    );
}

fn wait_for_account_job(
    actions: &mut actions::AccountActions,
    id: &str,
) -> actions::AccountJobState {
    let until = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        let job = actions
            .status()
            .into_iter()
            .find(|job| job.id == id)
            .unwrap();
        if !matches!(job.state, actions::AccountJobState::Running) {
            return job.state;
        }
        assert!(
            std::time::Instant::now() < until,
            "account worker did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn local_account_actions_keep_private_pkce_state_and_return_only_sealed_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let target_home = temp.path().join("target-home");
    std::fs::create_dir_all(&target_home).unwrap();
    let root = target_home.join(".config/tilo");
    let server = tilo_oauth::fake::FakeAuthorizationServer::start();
    let custodian = account_custody::Custodian::for_root(Some(root.clone())).with_provider(
        account_custody::OAuthCloud::GoogleDrive,
        server.provider(),
        "tilo-client",
    );
    let mut actions = actions::AccountActions::default().with_custodian(custodian);
    let consent = actions
        .begin(actions::ConnectionProvider::GoogleDrive, None, &target_home)
        .unwrap();
    assert!(actions.busy());
    assert_eq!(
        actions.update_sources(
            SourceRoots {
                home: temp.path().join("source"),
                config: temp.path().join("source/.config")
            },
            Inventory::default()
        ),
        Err(ActionError::Busy)
    );
    let public = serde_json::to_string(&consent).unwrap();
    assert!(public.contains("code_challenge"));
    assert!(!public.contains("code_verifier"));
    let browser = std::thread::spawn(move || tilo_oauth::fake::browse(&consent.url));
    let job = actions.complete(&consent.connection_id).unwrap();
    let outcome = wait_for_account_job(&mut actions, &job);
    browser.join().unwrap().unwrap();
    let actions::AccountJobState::Completed { account_id } = outcome else {
        panic!("connection failed");
    };
    assert!(
        SecretVault::open_in(&root)
            .get(&AccountId::new(account_id))
            .is_some()
    );
    assert!(!actions.busy());
    let receipt = serde_json::to_string(&actions.status()).unwrap();
    assert!(
        !receipt.contains("refresh_token")
            && !receipt.contains("access_token")
            && !receipt.contains("code_verifier")
    );
}

#[cfg(unix)]
#[test]
fn account_actions_refuse_copied_config_symlinks_before_creating_connections() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    let old_config = temp.path().join("old-config");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::create_dir_all(&old_config).unwrap();
    std::os::unix::fs::symlink(&old_config, target.join(".config")).unwrap();
    let mut actions = actions::AccountActions::default();
    assert_eq!(
        actions
            .begin(actions::ConnectionProvider::GoogleDrive, None, &target)
            .unwrap_err(),
        actions::ActionError::TargetRedirected
    );
    assert_eq!(std::fs::read_dir(old_config).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn account_actions_refuse_destination_home_symlink_itself() {
    let temp = tempfile::tempdir().unwrap();
    let old_home = temp.path().join("old-home");
    let target_home = temp.path().join("destination-link");
    std::fs::create_dir_all(&old_home).unwrap();
    std::os::unix::fs::symlink(&old_home, &target_home).unwrap();
    let mut actions = AccountActions::default();
    assert_eq!(
        actions
            .begin(ConnectionProvider::GoogleDrive, None, &target_home)
            .unwrap_err(),
        ActionError::TargetRedirected
    );
    assert_eq!(std::fs::read_dir(old_home).unwrap().count(), 0);
}

#[test]
fn selected_account_import_refuses_source_overlap_and_leaves_original_bytes_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    let path = config.join("rclone/rclone.conf");
    let original = "[Files]\ntype=webdav\nurl=https://files.example.test/dav\nuser=ada\npass=AAECAwQFBgcICQoLDA0OD2f2H1FyoCWnrFF4SHSBvFFHjlw9\n";
    std::fs::write(&path, original).unwrap();
    let roots = SourceRoots {
        home: home.clone(),
        config,
    };
    let inventory = discover(&roots);
    let id = inventory.candidates[0].id.clone();
    let mut actions = AccountActions::reviewed(roots, inventory);
    let child = home.join("destination");
    std::fs::create_dir_all(&child).unwrap();
    for target in [home.as_path(), temp.path(), child.as_path()] {
        assert_eq!(
            actions.import_selected(&id, target),
            Err(ActionError::TargetOverlapsSource)
        );
    }
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    assert!(!home.join(".config/tilo").exists());
    assert!(!actions.busy());
}

#[cfg(unix)]
#[test]
fn account_actions_refuse_a_destination_writable_by_other_users() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("shared-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        AccountActions::default()
            .begin(ConnectionProvider::GoogleDrive, None, &home)
            .unwrap_err(),
        ActionError::TargetNotPrivate
    );
    assert!(!home.join(".config").exists());
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::create_dir(home.join(".config")).unwrap();
    std::fs::set_permissions(home.join(".config"), std::fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        AccountActions::default()
            .begin(ConnectionProvider::GoogleDrive, None, &home)
            .unwrap_err(),
        ActionError::TargetNotPrivate
    );
    assert!(!home.join(".config/tilo").exists());
}

fn connect_spotify_test_slots(connector: &Connector) -> Vec<AccountId> {
    let mut selected = Vec::new();
    for playback in [false, true] {
        let authorization = if playback {
            connector.begin_spotify_streaming().unwrap()
        } else {
            connector.begin_spotify().unwrap()
        };
        let url = authorization.url().to_owned();
        let parsed = url::Url::parse(&url).unwrap();
        let query: std::collections::BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(query["client_id"], "tilo-test-registered-client");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(
            query["scope"],
            if playback {
                "streaming user-read-private"
            } else {
                "user-read-private user-library-read"
            }
        );
        let browser = std::thread::spawn(move || tilo_oauth::fake::browse(&url));
        let id = if playback {
            connector.finish_spotify_streaming(authorization).unwrap()
        } else {
            connector.finish_spotify(authorization).unwrap()
        };
        browser.join().unwrap().unwrap();
        assert_eq!(
            id.as_str(),
            if playback {
                "music.spotify.streaming"
            } else {
                "music.spotify"
            }
        );
        selected.push(id);
    }
    selected
}

#[test]
fn spotify_library_and_playback_use_distinct_pkce_grants_and_survive_prepared_export() {
    use account_custody::{Capability, ClientTable, Custodian, OAuthIssuer, OAuthUse};
    let temp = tempfile::tempdir().unwrap();
    let stage = temp.path().join("stage");
    let destination = temp.path().join("destination");
    let server = tilo_oauth::fake::FakeAuthorizationServer::start();
    let table = ClientTable::parse(
        r#"
[spotify]
client_id = "tilo-test-registered-client"
[spotify.scopes]
music = ["user-read-private", "user-library-read"]
music_streaming = ["streaming", "user-read-private"]
"#,
    )
    .unwrap();
    let custodian = Custodian::for_root(Some(stage.clone()))
        .with_client_table(table)
        .with_issuer_endpoints(OAuthIssuer::Spotify, server.provider());
    let profile =
        serde_json::json!({"id":"ada-spotify","account_id":"stable-ada","display_name":"Ada"});
    let (endpoint, api) =
        super::spotify_identity::profile_server(vec![profile.clone(), profile.clone(), profile]);
    let mut connector = Connector::with_custodian(stage.clone(), custodian);
    connector.spotify_api = Some(endpoint);
    let selected = connect_spotify_test_slots(&connector);
    api.join().unwrap();
    let store = AccountStore::load_in(&stage);
    for id in &selected {
        assert_eq!(
            store.get(id).unwrap().config["spotify_user_id"],
            "ada-spotify"
        );
        assert_eq!(
            store.get(id).unwrap().config["spotify_account_id"],
            "stable-ada"
        );
    }
    let source_vault = std::fs::read(accounts::vault_dir_in(&stage).join("vault.enc")).unwrap();
    let password = b"a long prepared music password";
    let sealed = connector
        .seal_selected_accounts_with_password(&selected, password, b"music-setup")
        .unwrap();
    let target = Connector::new(destination.clone());
    target
        .import_password_sealed_accounts_matching(&sealed, password, b"music-setup", &selected)
        .unwrap();
    let imported = Custodian::for_root(Some(destination.clone()));
    let [library, playback] = selected.as_slice() else {
        panic!("both independently consented Spotify slots must be present");
    };
    for (id, capability, expected_scope) in [
        (
            library,
            Capability::Music,
            "user-read-private user-library-read",
        ),
        (
            playback,
            Capability::MusicStreaming,
            "streaming user-read-private",
        ),
    ] {
        assert_eq!(
            imported.oauth_scopes(id).unwrap().as_deref(),
            Some(expected_scope)
        );
        assert!(
            !imported
                .oauth_access_token(id, OAuthUse::new(OAuthIssuer::Spotify, capability), false)
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&stage).join("vault.enc")).unwrap(),
        source_vault
    );
    let metadata = std::fs::read_to_string(accounts::store_path_in(&destination)).unwrap();
    assert!(!metadata.contains("refresh_token"));
    assert!(!metadata.contains("access_token"));
}

#[test]
fn spotify_playback_is_available_only_with_its_canonical_configured_scopes() {
    use account_custody::{ClientTable, Custodian};
    let no_playback = ClientTable::parse(
        r#"
[spotify]
client_id = "tilo-test-registered-client"
[spotify.scopes]
music = ["user-read-private", "user-library-read"]
"#,
    )
    .unwrap();
    let actions = AccountActions::default()
        .with_custodian(Custodian::for_root(None).with_client_table(no_playback));
    assert!(!actions.connection_available(ConnectionProvider::SpotifyStreaming));
    assert!(actions.connection_available(ConnectionProvider::Spotify));
}

#[test]
fn copied_tilo_cloud_grant_refresh_and_portable_import_keep_source_sign_in_nonrevocable() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    let target = temporary.path().join("target");
    let id = AccountId::new("source-google");
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    issuer.accept_refresh_token("source-refresh");
    let tokens = account_custody::TokenSet {
        refresh_token: "source-refresh".into(),
        ..Default::default()
    };
    let original_secret = account_custody::oauth_secret(&tokens);
    let mut vault = SecretVault::open_in(&source);
    vault.put(&id, &original_secret);
    vault.try_save().unwrap();
    let mut source_store = AccountStore::new();
    source_store.upsert(Account::new(id.clone(), "google-drive"));
    source_store.save_in(&source).unwrap();
    let source_bytes = std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    let connector = Connector::new(target.clone());
    assert_eq!(connector.import_tilo(&source, &id).unwrap(), id);
    assert_eq!(connector.import_tilo(&source, &id).unwrap(), id);
    let imported_secret = SecretVault::open_in(&target).get(&id).unwrap();
    assert_eq!(
        imported_secret,
        account_custody::copied_secret(source_store.get(&id).unwrap(), &original_secret)
    );
    assert!(account_custody::is_copied_secret(&imported_secret));
    let custodian = account_custody::Custodian::for_root(Some(target.clone())).with_provider(
        account_custody::OAuthCloud::GoogleDrive,
        issuer.provider(),
        "tilo-client",
    );
    custodian
        .oauth_access_token(
            &id,
            account_custody::OAuthCloud::GoogleDrive.oauth_use(),
            true,
        )
        .unwrap();
    assert!(account_custody::is_copied_secret(
        &SecretVault::open_in(&target).get(&id).unwrap()
    ));
    assert_eq!(connector.import_tilo(&source, &id).unwrap(), id);
    let key = tilo_seal::SymmetricKey::generate().unwrap();
    let sealed = connector
        .seal_selected_accounts(std::slice::from_ref(&id), &key, b"source-copy")
        .unwrap();
    let restored_root = temporary.path().join("restored");
    Connector::new(restored_root.clone())
        .import_sealed_accounts(&sealed, &key, b"source-copy")
        .unwrap();
    let restored_secret = SecretVault::open_in(&restored_root).get(&id).unwrap();
    assert!(account_custody::is_copied_secret(&restored_secret));
    let account = AccountStore::load_in(&restored_root)
        .get(&id)
        .cloned()
        .unwrap();
    custodian.revoke(&account, &restored_secret);
    assert!(issuer.revoked().is_empty());
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        source_bytes
    );
    assert_eq!(
        SecretVault::open_read_only_in(&source).get(&id).unwrap(),
        original_secret
    );
    // An independent newly-authorized prepared grant retains revocation on handoff.
    let fresh_target = temporary.path().join("fresh-prepared-target");
    Connector::new(fresh_target.clone())
        .handoff_prepared(&source, &id)
        .unwrap();
    let handed_off = SecretVault::open_in(&fresh_target).get(&id).unwrap();
    assert_eq!(handed_off, original_secret);
    assert!(!account_custody::is_copied_secret(&handed_off));
    custodian.revoke(&account, &handed_off);
    assert!(!issuer.revoked().is_empty());
}

#[test]
fn copied_nextcloud_app_password_detaches_without_revoke_but_fresh_handoff_keeps_revoke() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&calls);
    let server = test_http::Responder::handle(move |request| {
        assert_eq!(request.method, "DELETE");
        assert!(request.target.contains("/ocs/v2.php/core/apppassword"));
        observed.fetch_add(1, Ordering::SeqCst);
        Some(test_http::Reply::status(200))
    });
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("original");
    let id = AccountId::new("existing-nextcloud");
    let mut account = Account::new(id.clone(), "nextcloud");
    account.config = serde_json::json!({"server_url":server.url(), "username":"ada", "app_password":true, "use_files":true});
    let mut store = AccountStore::new();
    store.upsert(account);
    store.save_in(&source).unwrap();
    let secret = Secret::password("original-app-password");
    let mut vault = SecretVault::open_in(&source);
    vault.put(&id, &secret);
    vault.try_save().unwrap();
    let source_bytes = std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    let copied = Connector::new(temporary.path().join("copied"));
    copied.import_tilo(&source, &id).unwrap();
    copied.disconnect(&id).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        SecretVault::open_read_only_in(&source).get(&id).unwrap(),
        secret
    );
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        source_bytes
    );
    // Even an unknown future tagged grant with an outer password cannot be
    // mistaken for a fresh Nextcloud app password by the current custodian.
    let mut unknown = secret.clone();
    unknown.token = Some(serde_json::json!({"kind":"future_protected_password"}).to_string());
    let account = AccountStore::load_in(&source).get(&id).cloned().unwrap();
    assert!(account_custody::password_credential(&unknown).is_empty());
    account_custody::Custodian::for_root(Some(source.clone())).revoke(&account, &unknown);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let fresh = Connector::new(temporary.path().join("fresh-prepared"));
    fresh.handoff_prepared(&source, &id).unwrap();
    fresh.disconnect(&id).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn copied_password_retry_refuses_changed_target_password_without_overwriting_or_source_changes() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    let target = temporary.path().join("target");
    let id = AccountId::new("copied-dav");
    let mut store = AccountStore::new();
    store.upsert(Account::new(id.clone(), "webdav"));
    store.save_in(&source).unwrap();
    let mut source_vault = SecretVault::open_in(&source);
    source_vault.put(&id, &Secret::password("original-password"));
    source_vault.try_save().unwrap();
    let original_bytes = std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    let connector = Connector::new(target.clone());
    connector.import_tilo(&source, &id).unwrap();
    let mut target_vault = SecretVault::open_in(&target);
    let mut changed = target_vault.get(&id).unwrap();
    let mut grant: serde_json::Value =
        serde_json::from_str(changed.token.as_deref().unwrap()).unwrap();
    grant["password"] = "edited-target-password".into();
    changed.token = Some(serde_json::to_string(&grant).unwrap());
    assert_eq!(
        account_custody::password_credential(&changed).as_str(),
        "edited-target-password"
    );
    target_vault.put(&id, &changed);
    target_vault.try_save().unwrap();
    assert_eq!(
        connector.import_tilo(&source, &id),
        Err(ConnectionError::AlreadyPresent)
    );
    assert_eq!(SecretVault::open_in(&target).get(&id).unwrap(), changed);
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        original_bytes
    );
    assert_eq!(
        account_custody::password_credential(
            &SecretVault::open_read_only_in(&source).get(&id).unwrap()
        )
        .to_string(),
        "original-password"
    );
}
