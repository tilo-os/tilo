//! The stored form of an iCloud session: what custody seals in the vault.

#![allow(clippy::unwrap_used, reason = "test fixtures")]

use base64::Engine as _;
use cloud_source_icloud::auth::StoredSession;
use cloud_source_icloud::fake::FakeICloud;

#[test]
fn stored_sessions_round_trip_and_never_print_secrets() {
    let server = FakeICloud::start();
    let session = server.session();
    let json = session.to_json();
    let back = StoredSession::from_json(&json).unwrap();
    assert_eq!(back.trust_token(), session.trust_token());
    let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
    let decoded = StoredSession::from_json(&encoded).unwrap();
    assert_eq!(decoded.drive_url(), session.drive_url());
    let printed = format!("{session:?} {:?}", session.credential());
    assert!(!printed.contains(session.trust_token()));
    assert!(!printed.contains("wt-"), "{printed}");
    assert!(StoredSession::from_json("not a session").is_err());
}

/// Sessions sealed before sign-in moved to custody still open, and
/// serialize back byte for byte.
#[test]
fn the_sealed_format_is_unchanged() {
    let json = r#"{"apple_id":"ada@example.com","dsid":"12345","account_country":"USA","session_token":"st-1","trust_token":"tt-2","cookies":[["X-APPLE-WEBAUTH-TOKEN","\"v=2:t=wt-3\""]],"drive_url":"https://p1-drivews.icloud.com","docs_url":"https://p1-docws.icloud.com"}"#;
    let session = StoredSession::from_json(json).unwrap();
    assert_eq!(session.session_token(), "st-1");
    assert_eq!(session.account_country(), "USA");
    assert_eq!(session.to_json().as_str(), json);
}
