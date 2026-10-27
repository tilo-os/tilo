//! The whole browser round trip against [`crate::fake`]: loopback redirect,
//! PKCE verification by the server, `state` validation, refresh and
//! revocation.

use std::time::Duration;

use crate::client::{Authorization, OAuthClient};
use crate::fake::{FakeAuthorizationServer, browse};
use crate::{OAuthError, RedirectPorts, Registration, TokenSet};

const WAIT: Duration = Duration::from_secs(20);

fn registration() -> Registration {
    Registration::public("client-1")
}

fn sign_in(server: &FakeAuthorizationServer) -> Result<TokenSet, OAuthError> {
    let provider = server.provider();
    let authorization = Authorization::begin(&provider, &registration(), Some("ada@example.test"))?;
    let url = authorization.url().to_string();
    let browser = std::thread::spawn(move || browse(&url));
    let result = authorization.complete(&OAuthClient::new()?, WAIT);
    browser
        .join()
        .expect("browser thread")
        .expect("browser round trip");
    result
}

#[test]
fn code_flow_with_pkce_yields_tokens() {
    let server = FakeAuthorizationServer::start();
    let tokens = sign_in(&server).expect("sign-in");
    assert!(tokens.access_token.starts_with("access-"));
    assert!(tokens.refresh_token.starts_with("refresh-"));
    assert_eq!(tokens.expires_in, 3600);

    let requests = server.requests();
    let authorize = requests
        .iter()
        .find(|request| request.target.starts_with("/authorize?"))
        .expect("the browser visited the authorization endpoint");
    assert!(authorize.target.contains("code_challenge_method=S256"));
    assert!(authorize.target.contains("access_type=offline"));
    assert!(authorize.target.contains("login_hint=ada%40example.test"));
    assert!(
        authorize
            .target
            .contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"),
        "loopback redirect on 127.0.0.1: {}",
        authorize.target
    );
    // The verifier itself never travels on the authorization request; the
    // server saw it only at the token endpoint, and it matched.
    let verifiers = server.verifiers();
    assert_eq!(verifiers.len(), 1);
    assert!(!authorize.target.contains(&verifiers[0]));
}

#[test]
fn a_code_redeemed_with_the_wrong_verifier_is_rejected() {
    let server = FakeAuthorizationServer::start();
    let provider = server.provider();
    let authorization = Authorization::begin(&provider, &registration(), None).unwrap();
    // Intercept the code the way a malicious app on the same machine would,
    // by asking the server for it with our challenge, then try to redeem it
    // without knowing the verifier.
    let location = {
        let url = authorization.url().to_string();
        let mut rebuilt = url::Url::parse(&url).unwrap();
        let mut query: Vec<(String, String)> = rebuilt.query_pairs().into_owned().collect();
        for (key, value) in &mut query {
            if key == "redirect_uri" {
                *value = format!("{}/nowhere", server.url());
            }
        }
        rebuilt.query_pairs_mut().clear().extend_pairs(query);
        rebuilt.to_string()
    };
    browse(&location).unwrap();
    let requests = server.requests();
    let stolen = requests
        .iter()
        .find_map(|request| {
            request
                .target
                .strip_prefix("/nowhere?code=")
                .map(|rest| rest.split('&').next().unwrap_or_default().to_string())
        })
        .expect("the code was delivered");
    let client = OAuthClient::new().unwrap();
    assert_eq!(
        client.exchange_code(
            &provider,
            &registration(),
            Some(&format!("{}/nowhere", server.url())),
            &stolen,
            "not-the-verifier-for-this-challenge-0000000000",
        ),
        Err(OAuthError::InvalidGrant)
    );
}

#[test]
fn a_forged_state_is_refused_before_any_token_request() {
    let server = FakeAuthorizationServer::start();
    server.forge_state("attacker-state");
    assert_eq!(sign_in(&server), Err(OAuthError::StateMismatch));
    assert!(
        server.verifiers().is_empty(),
        "no code exchange after a state mismatch"
    );
}

#[test]
fn refresh_renews_the_access_token_and_keeps_the_refresh_token() {
    let server = FakeAuthorizationServer::start();
    server.set_token_lifetime(1);
    let tokens = sign_in(&server).unwrap();
    assert!(tokens.needs_refresh(), "a one second token is due");
    let client = OAuthClient::new().unwrap();
    let renewed = client
        .refresh(&server.provider(), &registration(), &tokens.refresh_token)
        .unwrap();
    assert_ne!(renewed.access_token, tokens.access_token);
    assert_eq!(
        renewed.refresh_token, tokens.refresh_token,
        "carried forward when the provider does not rotate it"
    );
    assert_eq!(server.refreshes(), 1);
}

#[test]
fn a_revoked_refresh_token_is_invalid_grant() {
    let server = FakeAuthorizationServer::start();
    let tokens = sign_in(&server).unwrap();
    server.revoke_all_refresh_tokens();
    let error = OAuthClient::new()
        .unwrap()
        .refresh(&server.provider(), &registration(), &tokens.refresh_token)
        .unwrap_err();
    assert_eq!(error, OAuthError::InvalidGrant);
    assert!(error.needs_authentication());
}

#[test]
fn revocation_sends_the_refresh_token_to_the_revocation_endpoint() {
    let server = FakeAuthorizationServer::start();
    let tokens = sign_in(&server).unwrap();
    let client = OAuthClient::new().unwrap();
    assert_eq!(
        client.revoke(&server.provider(), &registration(), &tokens),
        Ok(true)
    );
    assert_eq!(server.revoked(), vec![tokens.refresh_token.clone()]);
    let mut provider = server.provider();
    provider.revocation = crate::Revocation::Unsupported;
    assert_eq!(
        client.revoke(&provider, &registration(), &tokens),
        Ok(false)
    );
}

#[test]
fn cancelling_ends_a_pending_sign_in() {
    let server = FakeAuthorizationServer::start();
    let authorization = Authorization::begin(&server.provider(), &registration(), None).unwrap();
    authorization.canceller().expect("a loopback wait").cancel();
    assert_eq!(
        authorization.complete(&OAuthClient::new().unwrap(), WAIT),
        Err(OAuthError::Cancelled)
    );
}

#[test]
fn the_client_secret_goes_to_the_token_endpoint() {
    let server = FakeAuthorizationServer::start();
    server.require_client_secret("installed-app-secret");
    assert!(matches!(sign_in(&server), Err(OAuthError::Protocol(_))));
    let registration = Registration {
        client_secret: Some("installed-app-secret".into()),
        ..registration()
    };
    let authorization = Authorization::begin(&server.provider(), &registration, None).unwrap();
    let url = authorization.url().to_string();
    let browser = std::thread::spawn(move || browse(&url));
    let tokens = authorization
        .complete(&OAuthClient::new().unwrap(), WAIT)
        .expect("sign-in with the secret");
    browser.join().unwrap().unwrap();
    OAuthClient::new()
        .unwrap()
        .refresh(&server.provider(), &registration, &tokens.refresh_token)
        .expect("refresh with the secret");
}

#[test]
fn registered_ports_are_taken_in_order_and_the_code_can_be_pasted_when_all_are_busy() {
    // The fake server binds its own port first, so it cannot take the one
    // freed below. Two ports the OS just handed out: hold the first so the
    // listener has to take the second, released only right before it binds.
    let server = FakeAuthorizationServer::start();
    let first = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let second = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let first_port = first.local_addr().unwrap().port();
    let second_port = second.local_addr().unwrap().port();
    let registration = Registration {
        redirect_ports: RedirectPorts::Registered(vec![first_port, second_port]),
        paste_code_fallback: true,
        ..registration()
    };
    drop(second);
    let authorization = Authorization::begin(&server.provider(), &registration, None).unwrap();
    assert_eq!(
        authorization.redirect_uri(),
        Some(format!("http://127.0.0.1:{second_port}/oauth"))
    );
    drop(authorization);

    // Every registered port busy: the provider displays the code instead.
    let busy = Registration {
        redirect_ports: RedirectPorts::Registered(vec![first_port]),
        ..registration
    };
    let authorization = Authorization::begin(&server.provider(), &busy, None).unwrap();
    assert!(authorization.needs_pasted_code());
    assert!(!authorization.url().contains("redirect_uri"));
    let code = browse(authorization.url()).expect("the provider shows the code");
    let tokens = authorization
        .complete_with_code(&OAuthClient::new().unwrap(), &code)
        .expect("a pasted code is redeemed");
    assert!(!tokens.refresh_token.is_empty());

    // Without the fallback, busy ports are an error.
    let strict = Registration {
        paste_code_fallback: false,
        ..busy
    };
    assert!(matches!(
        Authorization::begin(&server.provider(), &strict, None),
        Err(OAuthError::RedirectPortsBusy)
    ));
    drop(first);
}
