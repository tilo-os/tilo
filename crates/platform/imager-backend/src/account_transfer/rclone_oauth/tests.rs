use super::*;
use accounts::AccountStore;

#[test]
fn imported_rclone_registration_refreshes_after_portable_reseal_without_touching_source() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("source/.config");
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    let original = "[Drive]\ntype=drive\nclient_id=original-rclone-client\nclient_secret=verbatim-source-secret\nroot_folder_id=original-folder\ntoken={\"access_token\":\"old\",\"refresh_token\":\"source-refresh\",\"expiry\":\"2000-01-01T00:00:00Z\"}\n";
    let path = config.join("rclone/rclone.conf");
    std::fs::write(&path, original).unwrap();
    let roots = SourceRoots {
        home: temp.path().join("source"),
        config,
    };
    let inventory = super::super::discover(&roots);
    let selected = &inventory.candidates[0];
    assert!(selected.can_import_credentials);
    let metadata = serde_json::to_string(&inventory).unwrap();
    for secret in ["verbatim-source-secret", "source-refresh", "access_token"] {
        assert!(!metadata.contains(secret));
    }
    let server = tilo_oauth::fake::FakeAuthorizationServer::start();
    server.require_client_secret("verbatim-source-secret");
    server.accept_refresh_token("source-refresh");
    let target = temp.path().join("target");
    let custodian = account_custody::Custodian::for_root(Some(target.clone())).with_provider(
        OAuthCloud::GoogleDrive,
        server.provider(),
        "different-tilo-client",
    );
    let connector = Connector::with_custodian(target.clone(), custodian);
    let id = connector.import_rclone(&roots, selected).unwrap();
    assert_eq!(server.refreshes(), 1);
    assert_eq!(connector.import_rclone(&roots, selected).unwrap(), id);
    assert_eq!(server.refreshes(), 1);
    let store = AccountStore::load_in(&target);
    assert_eq!(
        store.get(&id).unwrap().config[accounts::cloud_types::MOUNT_CONFIG_KEY]["root"],
        "original-folder"
    );
    let account_json = std::fs::read_to_string(accounts::store_path_in(&target)).unwrap();
    assert!(!account_json.contains("verbatim-source-secret"));
    let key = tilo_seal::SymmetricKey::generate().unwrap();
    let sealed = connector
        .seal_selected_accounts(std::slice::from_ref(&id), &key, b"rclone-transport")
        .unwrap();
    let restored_root = temp.path().join("restored");
    let restored = Connector::new(restored_root.clone());
    restored
        .import_sealed_accounts(&sealed, &key, b"rclone-transport")
        .unwrap();
    let destination = account_custody::Custodian::for_root(Some(restored_root.clone()))
        .with_provider(
            OAuthCloud::GoogleDrive,
            server.provider(),
            "different-target-client",
        );
    destination
        .oauth_access_token(&id, OAuthCloud::GoogleDrive.oauth_use(), true)
        .unwrap();
    assert_eq!(server.refreshes(), 2);
    for request in server
        .requests()
        .iter()
        .filter(|r| r.target.starts_with("/token"))
    {
        let body = request.body_text();
        let form = url::form_urlencoded::parse(body.as_bytes()).collect::<BTreeMap<_, _>>();
        assert_eq!(
            form.get("client_id").map(|s| s.as_ref()),
            Some("original-rclone-client")
        );
        assert_eq!(
            form.get("client_secret").map(|s| s.as_ref()),
            Some("verbatim-source-secret")
        );
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    let secret = SecretVault::open_in(&restored_root).get(&id).unwrap();
    let registration = registration(
        OAuthCloud::GoogleDrive,
        &section(original, "Drive").unwrap(),
        hex::encode(Sha256::digest(b"source-refresh")),
    )
    .unwrap();
    assert!(account_custody::imported_oauth_matches(
        &secret,
        &registration
    ));
    assert!(matches!(
        destination.oauth_access_token(
            &id,
            account_custody::OAuthUse::new(
                account_custody::OAuthIssuer::Microsoft,
                account_custody::Capability::Files
            ),
            false
        ),
        Err(account_custody::CustodyError::NeedsAuthentication)
    ));
    destination.revoke(store.get(&id).unwrap(), &secret);
    assert!(server.revoked().is_empty());
}

#[test]
fn unsupported_rclone_flows_and_reviewed_registration_changes_are_refused_before_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("source/.config");
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    let path = config.join("rclone/rclone.conf");
    let text =
        "[Drive]\ntype=drive\nclient_id=reviewed-client\ntoken={\"refresh_token\":\"private\"}\n";
    std::fs::write(&path, text).unwrap();
    let roots = SourceRoots {
        home: temp.path().join("source"),
        config,
    };
    let selected = super::super::discover(&roots).candidates.remove(0);
    let connector = Connector::new(temp.path().join("target"));
    std::fs::write(&path, text.replace("reviewed-client", "different-client")).unwrap();
    assert_eq!(
        connector.import_rclone(&roots, &selected),
        Err(ConnectionError::AccountMissing)
    );
    assert!(!connector.root.exists());
    for unsupported in [
        "token_url=https://untrusted.example/token",
        "client_credentials=true",
        "tenant=foreign",
        "service_account_file=private.json",
    ] {
        std::fs::write(&path, format!("{text}{unsupported}\n")).unwrap();
        let candidate = super::super::discover(&roots).candidates.remove(0);
        assert!(!candidate.can_import_credentials);
        assert_eq!(
            connector.import_rclone(&roots, &candidate),
            Err(ConnectionError::AccountMissing)
        );
        assert!(!connector.root.exists());
    }
    let values = section(
        "[Drive]\ntype=drive\nclient_id=custom\nclient_secret=literal\n",
        "Drive",
    )
    .unwrap();
    assert!(registration(OAuthCloud::GoogleDrive, &values, "binding".into()).is_ok());
    assert_eq!(
        section("[Drive]\ntoken=a\ntoken=b\n", "Drive").err(),
        Some(ConnectionError::InvalidConfiguration)
    );
}

#[test]
fn rclone_reviewed_remote_root_changes_require_new_review_and_keep_distinct_custody() {
    let temporary = tempfile::tempdir().unwrap();
    let config = temporary.path().join("source/.config");
    std::fs::create_dir_all(config.join("rclone")).unwrap();
    let path = config.join("rclone/rclone.conf");
    let original = "[Drive]\ntype=drive\nclient_id=source-client\nroot_folder_id=first-root\nteam_drive=reviewed-shared-drive\ntoken={\"refresh_token\":\"source-refresh\"}\n";
    std::fs::write(&path, original).unwrap();
    let roots = SourceRoots {
        home: temporary.path().join("source"),
        config,
    };
    let selected = super::super::discover(&roots).candidates.remove(0);
    let issuer = tilo_oauth::fake::FakeAuthorizationServer::start();
    issuer.accept_refresh_token("source-refresh");
    let target = temporary.path().join("target");
    let connector = Connector::with_custodian(
        target.clone(),
        account_custody::Custodian::for_root(Some(target.clone())).with_provider(
            OAuthCloud::GoogleDrive,
            issuer.provider(),
            "different-client",
        ),
    );
    let first = connector.import_rclone(&roots, &selected).unwrap();
    let revised = original.replace("first-root", "second-root");
    std::fs::write(&path, &revised).unwrap();
    assert_eq!(
        connector.import_rclone(&roots, &selected),
        Err(ConnectionError::AccountMissing)
    );
    assert_eq!(issuer.refreshes(), 1);
    let reviewed = super::super::discover(&roots).candidates.remove(0);
    let second = connector.import_rclone(&roots, &reviewed).unwrap();
    assert_ne!(first, second);
    let store = AccountStore::load_in(&target);
    for (id, expected) in [(first, "first-root"), (second, "second-root")] {
        let mount = &store.get(&id).unwrap().config[accounts::cloud_types::MOUNT_CONFIG_KEY];
        assert_eq!(mount["root"], expected);
        assert_eq!(mount["shared_drive"], "reviewed-shared-drive");
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), revised);
}
