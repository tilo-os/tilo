//! External OpenSSL AES-CBC golden ciphertext, never live owner credentials.
use super::super::{SourceGrant, read_selected};
use super::*;
use browser_continuity::{Provider, sessions::Service};
use rusqlite::Connection;
use std::sync::Arc;

// Generated independently with hashlib.pbkdf2_hmac('sha1', password,
// b'saltysalt', 1, 16), followed by external AES-128-CBC encryption with
// IV 20202020202020202020202020202020 and PKCS#7 padding.
// Plaintext is raw-host SHA256 + UTF8
// for schema24; schema23 stores just UTF8. These are synthetic test values.
const V10: &str = "76313034cc1c321001cbd03eb0a32947b5884a9d41f7cdef55a6c01c47b36fc1f513692fcce0d4e82bf502bd189b0770c6523e8e84ba6c19b831b8973f474af7a271ac";
const V11: &str = "763131ee96ec8095b1bf04a8f9fa1c5a7d45308ea6eb722925cccd5f79ba1ba5f754d71a47efde835ff2fd5375a1c507ea6a313def214c97d1c7f3a16e8eb9119fbd19";
const V11_23: &str = "763131e13a0e822076151cad52880a2953bc9b7b75c5919b5f016f52db17787076816f";
const VALUE: &str = "synthetic-encrypted-session";

fn fixture(host: &str, encrypted: &str) -> (tempfile::TempDir, Connection, SourceGrant) {
    let directory = tempfile::tempdir().unwrap();
    let connection = Connection::open(directory.path().join("Cookies")).unwrap();
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT); INSERT INTO meta VALUES('version','24');
        CREATE TABLE cookies(creation_utc INTEGER,host_key TEXT,name TEXT,value TEXT,path TEXT,
        expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,has_expires INTEGER,
        encrypted_value BLOB,top_frame_site_key TEXT,source_scheme INTEGER,source_port INTEGER);").unwrap();
    connection
        .execute(
            "INSERT INTO cookies VALUES(1,?1,'selected','', '/',0,1,1,-1,0,?2,'',2,443)",
            rusqlite::params![host, hex::decode(encrypted).unwrap()],
        )
        .unwrap();
    connection.execute_batch("INSERT INTO cookies VALUES(2,'unselected.example','bad','', '/',0,0,0,999,0,X'0123','https://other.example',0,80);").unwrap();
    let grant =
        SourceGrant::new(File::open(directory.path()).unwrap(), Provider::Chromium).unwrap();
    (directory, connection, grant)
}
fn synthetic_adapter(grant: &SourceGrant) -> ChromiumLinux {
    let facts = grant.profile.metadata().unwrap();
    ChromiumLinux {
        profile: grant.profile.try_clone().unwrap(),
        identity: (facts.uid(), facts.dev(), facts.ino()),
        brand: ChromiumBrand::Chromium,
        // This substitutes only synthetic keyring material. Database capture,
        // decoder, host verification and selected-cookie validation are real.
        v11: Mutex::new(Some(derive(b"synthetic-owner-password"))),
    }
}
fn unchanged(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    ["Cookies", "Cookies-wal", "Cookies-shm"]
        .into_iter()
        .filter_map(|name| {
            std::fs::read(root.join(name))
                .ok()
                .map(|bytes| (name.into(), bytes))
        })
        .collect()
}

#[test]
fn actual_selected_reader_decrypts_external_v10_v11_vectors_without_source_writes() {
    for (host, encrypted, service, host_only) in [
        (".slack.com", V10, Service::Slack, false),
        ("discord.com", V11, Service::Discord, true),
    ] {
        let (directory, _connection, grant) = fixture(host, encrypted);
        assert_eq!(
            read_selected(&grant, service).unwrap_err(),
            SourceError::UnsupportedEncryption
        );
        let adapter = synthetic_adapter(&grant);
        let grant = grant.with_chromium_decryptor(Arc::new(adapter)).unwrap();
        let before = unchanged(directory.path());
        let captured = read_selected(&grant, service).unwrap();
        assert_eq!(captured.cookies.len(), 1);
        assert_eq!(captured.cookies[0].value, VALUE);
        assert_eq!(captured.cookies[0].host_only, host_only);
        assert_eq!(captured.cookies[0].expires_unix_seconds, None);
        assert_eq!(unchanged(directory.path()), before);
        assert!(!format!("{captured:?}").contains(VALUE));
    }
}
#[test]
fn actual_reader_refuses_changed_raw_host_partition_and_dual_values() {
    let (_directory, connection, grant) = fixture("discord.com", V11);
    let adapter = synthetic_adapter(&grant);
    let grant = grant.with_chromium_decryptor(Arc::new(adapter)).unwrap();
    connection
        .execute_batch("UPDATE cookies SET host_key='.discord.com' WHERE name='selected';")
        .unwrap();
    assert_eq!(
        read_selected(&grant, Service::Discord).unwrap_err(),
        SourceError::UnsupportedEncryption
    );
    connection.execute_batch("UPDATE cookies SET host_key='discord.com',top_frame_site_key='https://other.example' WHERE name='selected';").unwrap();
    assert_eq!(
        read_selected(&grant, Service::Discord).unwrap_err(),
        SourceError::UnsupportedPartition
    );
    connection.execute_batch("UPDATE cookies SET top_frame_site_key='',value='conflicting plaintext' WHERE name='selected';").unwrap();
    assert_eq!(
        read_selected(&grant, Service::Discord).unwrap_err(),
        SourceError::InvalidCookies
    );
}
#[test]
fn actual_reader_recognizes_prior_schema_and_refuses_foreign_profile_authority() {
    let (_directory, connection, grant) = fixture("discord.com", V11_23);
    connection
        .execute_batch("UPDATE meta SET value='23';")
        .unwrap();
    let adapter = synthetic_adapter(&grant);
    let grant = grant.with_chromium_decryptor(Arc::new(adapter)).unwrap();
    assert_eq!(
        read_selected(&grant, Service::Discord).unwrap().cookies[0].value,
        VALUE
    );
    let (_foreign, _db, other) = fixture("discord.com", V11);
    let foreign_adapter = synthetic_adapter(&other);
    assert!(
        grant
            .with_chromium_decryptor(Arc::new(foreign_adapter))
            .is_err()
    );
}
#[test]
fn external_vectors_refuse_wrong_keys_truncation_unknown_prefixes_and_host_hashes() {
    let (_directory, _connection, grant) = fixture("discord.com", V11);
    let adapter = synthetic_adapter(&grant);
    let encrypted = hex::decode(V11).unwrap();
    assert_eq!(
        &*adapter.decrypt("discord.com", 24, &encrypted).unwrap(),
        VALUE
    );
    assert!(adapter.decrypt(".discord.com", 24, &encrypted).is_err());
    assert!(
        adapter
            .decrypt("discord.com", 24, &encrypted[..encrypted.len() - 1])
            .is_err()
    );
    let mut unknown = encrypted.clone();
    unknown[..3].copy_from_slice(b"v20");
    assert_eq!(
        adapter.decrypt("discord.com", 24, &unknown).unwrap_err(),
        SourceError::UnsupportedEncryption
    );
    assert!(
        decode(
            &derive(b"wrong-owner-password"),
            &encrypted[3..],
            "discord.com",
            24
        )
        .is_err()
    );
    assert!(
        decode(
            &derive(b"synthetic-owner-password"),
            &encrypted[3..],
            "discord.com",
            25
        )
        .is_err()
    );
}
