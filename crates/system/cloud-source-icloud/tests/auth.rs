//! Signing in, two-factor codes, trust tokens, Advanced Data Protection and
//! session renewal, against the fake idmsa/setup endpoints.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine as _;
use cloud_source::{CloudSource, CredentialSource, Error, StaticCredential};
use cloud_source_icloud::ICloudDrive;
use cloud_source_icloud::auth::{
    AuthError, CodeDelivery, SessionCredentials, SessionStatus, SignIn, StoredSession,
};
use cloud_source_icloud::fake::{
    APPLE_ID, DEVICE_CODE, FakeICloud, Options, PASSWORD, PHONE_ID, SMS_CODE,
};

fn needs_code(outcome: SignIn) -> cloud_source_icloud::auth::PendingSignIn {
    match outcome {
        SignIn::NeedsCode(pending) => pending,
        SignIn::Complete(_) => panic!("expected a second factor"),
    }
}

fn drive(server: &FakeICloud, session: &StoredSession) -> ICloudDrive {
    ICloudDrive::new(
        server.config(),
        Arc::new(StaticCredential(session.credential())),
    )
    .unwrap()
}

#[test]
fn a_pushed_code_signs_in_and_a_wrong_one_can_be_retried() {
    let server = FakeICloud::start();
    let auth = server.authenticator();
    let mut pending = needs_code(auth.sign_in(APPLE_ID, PASSWORD, None).unwrap());
    assert_eq!(pending.delivery(), &CodeDelivery::TrustedDevice);
    assert_eq!(server.stats().pushes, 1, "the code was pushed");
    assert_eq!(pending.phones().len(), 1);

    assert!(matches!(
        auth.submit_code(&mut pending, "000000"),
        Err(AuthError::InvalidCode)
    ));
    let session = auth.submit_code(&mut pending, DEVICE_CODE).unwrap();
    assert!(session.has_trust_token());
    assert_eq!(session.apple_id(), APPLE_ID);
    assert!(session.drive_url().ends_with("/drivews"));
    let source = drive(&server, &session);
    assert!(source.list(&source.root()).unwrap().is_empty());
}

#[test]
fn apple_ids_are_matched_case_insensitively() {
    let server = FakeICloud::start();
    let auth = server.authenticator();
    let outcome = auth.sign_in("  Ada@Example.COM ", PASSWORD, None).unwrap();
    assert!(matches!(outcome, SignIn::NeedsCode(_)));
}

#[test]
fn an_sms_code_is_the_fallback() {
    let server = FakeICloud::start();
    let auth = server.authenticator();
    let mut pending = needs_code(auth.sign_in(APPLE_ID, PASSWORD, None).unwrap());
    auth.send_sms(&mut pending, PHONE_ID).unwrap();
    assert!(matches!(pending.delivery(), CodeDelivery::Phone { id, .. } if *id == PHONE_ID));
    assert_eq!(server.stats().texts, 1);
    assert!(matches!(
        auth.submit_code(&mut pending, DEVICE_CODE),
        Err(AuthError::InvalidCode)
    ));
    auth.submit_code(&mut pending, SMS_CODE).unwrap();
}

#[test]
fn accounts_without_trusted_devices_get_a_text() {
    let server = FakeICloud::with(Options {
        trusted_devices: false,
        ..Options::default()
    });
    let auth = server.authenticator();
    let mut pending = needs_code(auth.sign_in(APPLE_ID, PASSWORD, None).unwrap());
    assert!(matches!(pending.delivery(), CodeDelivery::Phone { .. }));
    assert_eq!(server.stats().pushes, 0);
    assert_eq!(server.stats().texts, 1);
    auth.submit_code(&mut pending, SMS_CODE).unwrap();
}

#[test]
fn a_code_accepted_with_409_still_signs_in() {
    let server = FakeICloud::with(Options {
        accept_code_with_conflict: true,
        ..Options::default()
    });
    server.sign_in();
}

#[test]
fn the_fallback_password_protocol_works() {
    let server = FakeICloud::with(Options {
        protocol: "s2k_fo",
        ..Options::default()
    });
    server.sign_in();
}

#[test]
fn wrong_passwords_and_unknown_accounts_are_refused() {
    let server = FakeICloud::start();
    let auth = server.authenticator();
    assert!(matches!(
        auth.sign_in(APPLE_ID, "hunter2", None),
        Err(AuthError::InvalidCredentials)
    ));
    assert!(matches!(
        auth.sign_in("nobody@example.com", PASSWORD, None),
        Err(AuthError::InvalidCredentials)
    ));
}

#[test]
fn a_trust_token_skips_the_second_factor() {
    let server = FakeICloud::start();
    let first = server.sign_in();
    let pushes = server.stats().pushes;
    let auth = server.authenticator();
    let SignIn::Complete(second) = auth
        .sign_in(APPLE_ID, PASSWORD, Some(first.trust_token()))
        .unwrap()
    else {
        panic!("a trusted sign-in needs no code");
    };
    assert_eq!(server.stats().pushes, pushes);
    let source = drive(&server, &second);
    source.list(&source.root()).unwrap();
}

#[test]
fn advanced_data_protection_is_reported_clearly() {
    let server = FakeICloud::with(Options {
        advanced_data_protection: true,
        ..Options::default()
    });
    let auth = server.authenticator();
    let mut pending = needs_code(auth.sign_in(APPLE_ID, PASSWORD, None).unwrap());
    let error = auth.submit_code(&mut pending, DEVICE_CODE).unwrap_err();
    assert!(
        matches!(error, AuthError::AdvancedDataProtection),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "iCloud web access is unavailable with Advanced Data Protection"
    );
}

#[test]
fn lapsed_web_cookies_renew_from_the_tokens() {
    let server = FakeICloud::start();
    let session = server.sign_in();
    server.expire_web_sessions();
    let stale = drive(&server, &session);
    assert!(matches!(
        stale.list(&stale.root()),
        Err(Error::Unauthorized)
    ));
    let renewed = server.authenticator().refresh(&session, None).unwrap();
    let source = drive(&server, &renewed);
    source.list(&source.root()).unwrap();
}

#[test]
fn a_lapsed_trust_is_session_expired_without_prompting_anyone() {
    let server = FakeICloud::start();
    let session = server.sign_in();
    server.expire_trust();
    let pushes = server.stats().pushes;
    let auth = server.authenticator();
    assert!(matches!(
        auth.refresh(&session, None),
        Err(AuthError::SessionExpired)
    ));
    assert!(matches!(
        auth.refresh(&session, Some(PASSWORD)),
        Err(AuthError::SessionExpired)
    ));
    assert_eq!(
        server.stats().pushes,
        pushes,
        "a background renewal never pushes a code"
    );
}

#[test]
fn a_valid_session_refreshes_to_itself() {
    let server = FakeICloud::start();
    let session = server.sign_in();
    let refreshed = server.authenticator().refresh(&session, None).unwrap();
    assert_eq!(refreshed.dsid(), Some("12345"));
}

#[test]
fn session_credentials_renew_after_a_rejection_and_then_ask_for_a_person() {
    let server = FakeICloud::start();
    let session = server.sign_in();
    let renewals = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&renewals);
    let credentials = SessionCredentials::new(
        server.authenticator(),
        session,
        None,
        Some(Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })),
    );
    let source = ICloudDrive::new(server.config(), Arc::new(credentials.clone())).unwrap();
    source.list(&source.root()).unwrap();

    server.expire_web_sessions();
    assert!(matches!(
        source.list(&source.root()),
        Err(Error::Unauthorized)
    ));
    source
        .list(&source.root())
        .expect("renewed from the session token");
    assert_eq!(renewals.load(Ordering::SeqCst), 1);
    assert_eq!(credentials.status(), SessionStatus::Active);

    server.expire_trust();
    assert!(matches!(
        source.list(&source.root()),
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        source.list(&source.root()),
        Err(Error::Unauthorized)
    ));
    assert_eq!(credentials.status(), SessionStatus::NeedsSignIn);
    assert!(matches!(credentials.credential(), Err(Error::Unauthorized)));
}

#[test]
fn stored_sessions_round_trip_and_never_print_secrets() {
    let server = FakeICloud::start();
    let session = server.sign_in();
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
