use super::*;
use crate::setup::account_transfer::{self, SourceRoots};
use imager_backend::browser_selection::Service;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn selected_website_session_preparation_is_separate_authenticated_custody() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let home = root.path().join("source");
    let profile = home.join(".config/chromium/Default");
    fs::create_dir_all(&profile).unwrap();
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    let database = rusqlite::Connection::open(profile.join("Cookies")).unwrap();
    database.execute_batch("CREATE TABLE meta(key LONGVARCHAR PRIMARY KEY,value LONGVARCHAR);INSERT INTO meta VALUES('version','24');CREATE TABLE cookies(creation_utc INTEGER,host_key TEXT,name TEXT,value TEXT,path TEXT,expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,has_expires INTEGER,encrypted_value BLOB,top_frame_site_key TEXT,source_scheme INTEGER,source_port INTEGER);INSERT INTO cookies VALUES(1,'.slack.com','d','selected-session-secret','/',0,1,1,0,0,X'','',2,443);").unwrap();
    drop(database);
    let before = fs::read(profile.join("Cookies")).unwrap();
    let inventory = account_transfer::discover(&SourceRoots {
        home: home.clone(),
        config: home.join(".config"),
    });
    let mut sources =
        super::super::service_sessions::Selection::discover(&home, &inventory.candidates).unwrap();
    let projection = sources.report();
    let id = projection["sources"][0]["id"].as_str().unwrap();
    sources.set(id, Service::Slack, true).unwrap();
    let identity = PreparedIdentity::create(
        &root.path().join("identity"),
        "Ada",
        [20, 80, 140],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let password = "a sufficiently long session transport password";
    let prepared = PreparedMedia::prepare_with_selections(
        &identity,
        None,
        MediaSelections {
            sessions: Some(sources.selected()),
            home_applications: false,
            home: None,
            retained: None,
            planned_direct: None,
            applications: None,
            browser: None,
        },
        Zeroizing::new(password.into()),
        "selected-session-operation".into(),
        None,
    )
    .unwrap();
    assert_eq!(prepared.service_session_protocol(), Some(1));
    assert!(prepared.account_custody_protocol().is_none());
    assert!(prepared.manifest.browser.is_none());
    assert!(prepared.manifest.account_ids.is_empty());
    let declaration = prepared.manifest.service_sessions.as_ref().unwrap();
    assert_eq!(declaration.sessions.len(), 1);
    let held_artifact = prepared.staging_artifact().unwrap();
    let review = super::super::super::prepared_setup::PreparedSetupReview::open(
        std::path::Path::new(&held_artifact.source_path),
        password.as_bytes(),
        &root.path().join("quarantine"),
        &AtomicBool::new(false),
    )
    .unwrap();
    review
        .import_service_sessions(&root.path().join("target"), password.as_bytes())
        .unwrap();
    let selected = account_custody::Custodian::for_root(Some(root.path().join("target")))
        .service_session(
            &accounts::AccountId::from(declaration.sessions[0].id.clone()),
            identity.public_key(),
            "selected-session-operation",
        )
        .unwrap();
    assert_eq!(selected.cookies[0].value, "selected-session-secret");
    assert!(selected.cookies[0].expires_unix_seconds.is_none());
    assert_eq!(fs::read(profile.join("Cookies")).unwrap(), before);
    assert!(
        !serde_json::to_string(declaration)
            .unwrap()
            .contains("selected-session-secret")
    );
}

#[test]
fn excluded_parent_accounts_prepares_authenticated_media_without_reading_or_declaring_session_material()
 {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let home = root.path().join("source");
    let profile = home.join(".config/chromium/Default");
    fs::create_dir_all(&profile).unwrap();
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    let database = rusqlite::Connection::open(profile.join("Cookies")).unwrap();
    database.execute_batch("CREATE TABLE meta(key LONGVARCHAR PRIMARY KEY,value LONGVARCHAR);
        INSERT INTO meta VALUES('version','24');
        CREATE TABLE cookies(creation_utc INTEGER,host_key TEXT,name TEXT,value TEXT,path TEXT,
        expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,has_expires INTEGER,
        encrypted_value BLOB,top_frame_site_key TEXT,source_scheme INTEGER,source_port INTEGER);
        INSERT INTO cookies VALUES(1,'.slack.com','d','private-parent-exclusion-sentinel','/',
            0,1,1,0,0,X'','',2,443);").unwrap();
    drop(database);
    let inventory = account_transfer::discover(&SourceRoots {
        home: home.clone(),
        config: home.join(".config"),
    });
    let sources =
        super::super::service_sessions::Selection::discover(&home, &inventory.candidates).unwrap();
    assert!(sources.selected_for_accounts(true).is_some());
    // After consent withdrawal, even an unreadable source cannot be touched by preparation.
    fs::remove_file(profile.join("Cookies")).unwrap();
    let identity = PreparedIdentity::create(
        &root.path().join("identity"),
        "Ada",
        [20, 80, 140],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let password = "a sufficiently long parent exclusion password";
    let prepared = PreparedMedia::prepare_with_selections(
        &identity,
        None,
        MediaSelections {
            sessions: sources.selected_for_accounts(false),
            home_applications: false,
            home: None,
            retained: None,
            planned_direct: None,
            applications: None,
            browser: None,
        },
        Zeroizing::new(password.into()),
        "parent-exclusion-operation".into(),
        None,
    )
    .unwrap();
    assert!(prepared.service_session_protocol().is_none());
    assert!(prepared.manifest.service_sessions.is_none());
    let artifact = prepared.staging_artifact().unwrap();
    let review = super::super::super::prepared_setup::PreparedSetupReview::open(
        std::path::Path::new(&artifact.source_path),
        password.as_bytes(),
        &root.path().join("review"),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(review.manifest().service_sessions.is_none());
    assert!(!profile.join("Cookies").exists());
    assert!(
        sources
            .selected_for_accounts(true)
            .unwrap()
            .capture(identity.public_key(), "renewed-parent-consent".into())
            .is_err(),
        "re-enabling never fabricates source material"
    );
}
