use super::*;
use std::os::unix::fs::symlink;

const SELECTED: &[u8] = b"_https://discord.com\0\x01token";
fn fixture() -> (tempfile::TempDir, engine::Database, DiscordOriginGrant) {
    let profile = tempfile::tempdir().unwrap();
    let directory = profile.path().join("Local Storage/leveldb");
    std::fs::create_dir_all(&directory).unwrap();
    let database = engine::Database::open(&directory, true).unwrap();
    database.put(b"VERSION", Some(b"1"));
    database.put(SELECTED, Some(b"\x01\"older-selected-value\""));
    database.put(
        b"_https://discord.com.evil.example\0\x01token",
        Some(b"\x01wrong-origin"),
    );
    database.put(
        b"_https://discord.com^0https://other.example\0\x01token",
        Some(b"\x01partitioned-value"),
    );
    database.put(
        b"_https://discord.com\0\x01unrelated",
        Some(b"\x01unselected-value"),
    );
    let grant = DiscordOriginGrant::new(File::open(profile.path()).unwrap()).unwrap();
    (profile, database, grant)
}
fn bytes(profile: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(profile.join("Local Storage/leveldb"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}
#[test]
fn actual_engine_selects_current_origin_value_preserves_raw_dom_and_original_store() {
    let (profile, database, grant) = fixture();
    database.compact();
    database.put(SELECTED, Some(b"\x01\"synthetic-current-value\""));
    let before = bytes(profile.path());
    let captured = read_selected(&grant).unwrap();
    assert_eq!(&*captured.value, "\"synthetic-current-value\"");
    assert_ne!(captured.source_snapshot_digest, [0; 32]);
    assert!(!format!("{captured:?}").contains("synthetic-current-value"));
    assert_eq!(bytes(profile.path()), before);
    assert!(
        before.keys().any(|name| {
            matches!(
                std::path::Path::new(name)
                    .extension()
                    .and_then(|ext| ext.to_str()),
                Some("ldb" | "sst")
            )
        }),
        "fixture must exercise real tables as well as WAL"
    );
}
#[test]
fn native_tombstone_never_resurrects_an_older_or_partitioned_token() {
    let (profile, database, grant) = fixture();
    database.compact();
    database.put(SELECTED, None);
    let before = bytes(profile.path());
    assert_eq!(read_selected(&grant).unwrap_err(), SourceError::Unavailable);
    assert_eq!(bytes(profile.path()), before);
}
#[test]
fn corrupt_current_wal_refuses_instead_of_resurrecting_an_old_secret() {
    let (profile, database, grant) = fixture();
    database.compact();
    database.put(SELECTED, None);
    let root = profile.path().join("Local Storage/leveldb");
    let log = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension() == Some(OsStr::new("log")))
        .max()
        .unwrap();
    let mut damaged = std::fs::read(&log).unwrap();
    *damaged
        .first_mut()
        .expect("real tombstone WAL must have a checksummed record") ^= 1;
    std::fs::write(log, &damaged).unwrap();
    let before = bytes(profile.path());
    assert_eq!(
        read_selected(&grant).unwrap_err(),
        SourceError::UnsupportedSchema
    );
    assert_eq!(bytes(profile.path()), before);
}
#[test]
fn utf16_dom_value_and_key_are_preserved_but_ambiguous_key_encodings_refuse() {
    let (_profile, database, grant) = fixture();
    database.put(SELECTED, None);
    let value = "\"synthetic-λ-value\"";
    let mut encoded = vec![0];
    for unit in value.encode_utf16() {
        encoded.extend_from_slice(&unit.to_le_bytes());
    }
    let key = b"_https://discord.com\0\x00t\0o\0k\0e\0n\0";
    database.put(key, Some(&encoded));
    assert_eq!(&*read_selected(&grant).unwrap().value, value);
    database.put(SELECTED, Some(b"\x01conflicting-value"));
    assert_eq!(
        read_selected(&grant).unwrap_err(),
        SourceError::InvalidCookies
    );
}
#[test]
fn symlinked_store_and_manifest_escape_are_not_opened_by_native_engine() {
    let (profile, database, grant) = fixture();
    let current = profile.path().join("Local Storage/leveldb/CURRENT");
    std::fs::write(current, b"../../outside\n").unwrap();
    assert_eq!(
        read_selected(&grant).unwrap_err(),
        SourceError::UnsupportedSchema
    );
    drop(database);
    let elsewhere = tempfile::tempdir().unwrap();
    let path = profile.path().join("Local Storage/leveldb");
    std::fs::remove_dir_all(&path).unwrap();
    symlink(elsewhere.path(), &path).unwrap();
    assert_eq!(read_selected(&grant).unwrap_err(), SourceError::Unavailable);
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}
#[test]
fn source_mutation_during_capture_refuses_changed_epoch() {
    let (_profile, database, grant) = fixture();
    let changed = snapshot_observed(&grant, || {
        database.put(SELECTED, Some(b"\x01changed-during-owner-capture"))
    });
    assert!(matches!(changed, Err(SourceError::Changed)));
    // The same production capture path succeeds once a stable epoch exists.
    let current = read_selected(&grant).unwrap();
    assert_eq!(&*current.value, "changed-during-owner-capture");
}

#[test]
fn origin_only_session_seals_imports_raw_dom_value_without_a_cookie_store() {
    use crate::setup::service_sessions::{self as sessions, PreparedSessions, SelectedSource};
    let (profile, database, grant) = fixture();
    let raw = "\"synthetic-private-λ-origin-session\"";
    let mut encoded = vec![0];
    for unit in raw.encode_utf16() {
        encoded.extend_from_slice(&unit.to_le_bytes());
    }
    database.put(SELECTED, Some(&encoded));
    assert!(!profile.path().join("Cookies").exists());
    let before = bytes(profile.path());
    let source_digest = read_selected(&grant).unwrap().source_snapshot_digest;
    let prepared = PreparedSessions::capture_selected(
        [7; 32],
        "origin-only-operation".into(),
        vec![SelectedSource::DiscordOrigin(grant)],
    )
    .unwrap();
    let password = b"a durable synthetic origin session password";
    let (artifact, declaration) = prepared.seal(password).unwrap();
    assert_eq!(declaration.consumer_protocol, 2);
    assert_eq!(declaration.sessions.len(), 1);
    assert_eq!(
        declaration.sessions[0].service,
        browser_continuity::sessions::Service::Discord
    );
    assert!(
        !serde_json::to_string(&declaration)
            .unwrap()
            .contains("synthetic-private")
    );
    assert!(
        !artifact
            .as_bytes()
            .windows(b"synthetic-private".len())
            .any(|part| part == b"synthetic-private")
    );
    drop(prepared);
    let mut downgraded = declaration.clone();
    downgraded.consumer_protocol = 1;
    assert!(
        sessions::validate_artifact(
            &artifact,
            &downgraded,
            password,
            [7; 32],
            "origin-only-operation",
        )
        .is_err()
    );
    let downgrade_target = sessions::private_stage().unwrap();
    assert!(
        sessions::import(
            &artifact,
            &downgraded,
            password,
            [7; 32],
            "origin-only-operation",
            downgrade_target.path(),
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_dir(downgrade_target.path()).unwrap().count(),
        0
    );
    let target = sessions::private_stage().unwrap();
    for (identity, binding) in [
        ([8; 32], "origin-only-operation"),
        ([7; 32], "another-operation"),
    ] {
        assert!(
            sessions::import(
                &artifact,
                &declaration,
                password,
                identity,
                binding,
                target.path()
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(target.path()).unwrap().count(), 0);
    }
    sessions::import(
        &artifact,
        &declaration,
        password,
        [7; 32],
        "origin-only-operation",
        target.path(),
    )
    .unwrap();
    let session = account_custody::Custodian::for_root(Some(target.path().to_owned()))
        .service_session(
            &accounts::AccountId::from(declaration.sessions[0].id.clone()),
            [7; 32],
            "origin-only-operation",
        )
        .unwrap();
    assert_eq!(session.schema, 2);
    assert!(session.cookies.is_empty());
    assert_eq!(session.origin_storage.len(), 1);
    assert_eq!(session.origin_storage[0].origin, ORIGIN);
    assert_eq!(session.origin_storage[0].key, KEY);
    assert_eq!(session.origin_storage[0].value, raw);
    assert_eq!(
        session.origin_storage[0].source_snapshot_digest,
        source_digest
    );
    assert_ne!(session.source_snapshot_digest, [0; 32]);
    assert!(!format!("{session:?}").contains("synthetic-private"));
    let mut changed = declaration;
    changed.ciphertext_sha256[0] ^= 1;
    let fresh = sessions::private_stage().unwrap();
    assert!(
        sessions::import(
            &artifact,
            &changed,
            password,
            [7; 32],
            "origin-only-operation",
            fresh.path()
        )
        .is_err()
    );
    assert_eq!(std::fs::read_dir(fresh.path()).unwrap().count(), 0);
    assert_eq!(bytes(profile.path()), before);
}

#[test]
fn selected_cookie_refusal_cannot_be_hidden_by_successful_origin_capture() {
    use crate::setup::service_sessions::{PreparedSessions, SelectedSource};
    let (profile, _database, grant) = fixture();
    std::fs::write(
        profile.path().join("Cookies"),
        b"not a valid selected cookie database",
    )
    .unwrap();
    let cookie = super::super::SourceGrant::new(
        File::open(profile.path()).unwrap(),
        browser_continuity::Provider::Chromium,
    )
    .unwrap();
    let before = bytes(profile.path());
    assert!(
        PreparedSessions::capture_selected(
            [7; 32],
            "selected-material-operation".into(),
            vec![
                SelectedSource::DiscordOrigin(grant),
                SelectedSource::Cookies(cookie, browser_continuity::sessions::Service::Discord)
            ],
        )
        .is_err()
    );
    assert_eq!(bytes(profile.path()), before);
    assert_eq!(
        std::fs::read(profile.path().join("Cookies")).unwrap(),
        b"not a valid selected cookie database"
    );
}

#[test]
fn explicitly_selected_discord_cookies_and_origin_share_one_sealed_session() {
    use crate::setup::service_sessions::{self as sessions, PreparedSessions, SelectedSource};
    let (profile, _database, grant) = fixture();
    let before = bytes(profile.path());
    let cookie_profile = tempfile::tempdir().unwrap();
    let cookie_path = cookie_profile.path().join("cookies.sqlite");
    let db = rusqlite::Connection::open(&cookie_path).unwrap();
    db.execute_batch("PRAGMA user_version=17; CREATE TABLE moz_cookies(id INTEGER PRIMARY KEY,host TEXT,name TEXT,value TEXT,path TEXT,expiry INTEGER,isSecure INTEGER,isHttpOnly INTEGER,sameSite INTEGER,originAttributes TEXT,isPartitionedAttributeSet INTEGER,inBrowserElement INTEGER); INSERT INTO moz_cookies VALUES(1,'discord.com','synthetic','synthetic-discord-cookie','/',2000000123000,1,1,256,'',0,0);").unwrap();
    drop(db);
    let cookie_before = std::fs::read(&cookie_path).unwrap();
    let cookie_grant = super::super::SourceGrant::new(
        File::open(cookie_profile.path()).unwrap(),
        browser_continuity::Provider::Firefox,
    )
    .unwrap();
    let prepared = PreparedSessions::capture_selected(
        [7; 32],
        "combined-operation".into(),
        vec![
            SelectedSource::Cookies(cookie_grant, browser_continuity::sessions::Service::Discord),
            SelectedSource::DiscordOrigin(grant),
        ],
    )
    .unwrap();
    let password = b"a durable combined synthetic session password";
    let (artifact, declaration) = prepared.seal(password).unwrap();
    assert_eq!(declaration.sessions.len(), 1);
    let target = sessions::private_stage().unwrap();
    sessions::import(
        &artifact,
        &declaration,
        password,
        [7; 32],
        "combined-operation",
        target.path(),
    )
    .unwrap();
    let session = account_custody::Custodian::for_root(Some(target.path().to_owned()))
        .service_session(
            &accounts::AccountId::from(declaration.sessions[0].id.clone()),
            [7; 32],
            "combined-operation",
        )
        .unwrap();
    assert_eq!(session.schema, 2);
    assert_eq!(session.cookies.len(), 1);
    assert_eq!(session.cookies[0].value, "synthetic-discord-cookie");
    assert_eq!(session.origin_storage.len(), 1);
    assert_eq!(session.origin_storage[0].value, "\"older-selected-value\"");
    assert_ne!(
        session.source_snapshot_digest,
        session.origin_storage[0].source_snapshot_digest
    );
    assert_eq!(std::fs::read(&cookie_path).unwrap(), cookie_before);
    assert_eq!(bytes(profile.path()), before);
}
