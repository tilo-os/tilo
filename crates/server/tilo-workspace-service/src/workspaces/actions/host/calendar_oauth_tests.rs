//! A calendar account signed in on its provider's page reads its server with
//! the grant that sign-in left. The holder sends the grant's access token as
//! a bearer token, renews it through the same web client the sign-in used
//! when it expired, and, when the provider will not renew it again, takes the
//! dead grant out of the vault: the account then lists as one whose person
//! signs in again, in words a person reads. A provider that was not reached
//! keeps its grant, and the refusal says to try again.

use super::*;
use tilo_oauth::TokenSet;
use tilo_oauth::fake::FakeAuthorizationServer;
use tilo_test_env::ScopedEnv;

/// The scope a Google calendar grant carries: what the sign-in asked for.
const CALENDAR_SCOPE: &str = "https://www.googleapis.com/auth/calendar";

/// The provider's page, as a local stand-in: the token endpoint every
/// renewal goes to, with the web client the server was signed in with.
fn web_client(env: &mut ScopedEnv, fake: &FakeAuthorizationServer) {
    env.set("TILO_OAUTH_GOOGLE_WEB_CLIENT_ID", "web-client")
        .set("TILO_OAUTH_GOOGLE_WEB_CLIENT_SECRET", "web-secret")
        .set(
            "TILO_OAUTH_GOOGLE_TOKEN_URL",
            format!("{}/token", fake.url()),
        );
}

/// The grant a sign-in on the provider's page leaves: its access token, the
/// token that renews it, and the lifetime the provider reported (`0` when it
/// reported none, which is a token that is renewed before every read).
fn grant(access: &str, refresh: &str, expires_in: u64) -> TokenSet {
    TokenSet {
        access_token: access.to_owned(),
        scope: Some(CALENDAR_SCOPE.to_owned()),
        refresh_token: refresh.to_owned(),
        expires_in,
        obtained_at: tilo_clock::unix_now(),
    }
}

/// Keep the calendar account a sign-in on Google's page keeps, and its
/// calendar as a device's Calendar keeps them, in the workspace's store: the
/// calendar server `server`, whose one calendar "work" is in it.
fn seed_google_calendar(root: &Path, server: &fake_caldav::Server, tokens: &TokenSet) -> String {
    let server_url = server.url();
    let address = tilo_dav::Url::parse(&server_url).expect("the server's address reads");
    let id = format!("caldav:{ADDRESS}@{}", address.authority());
    let mut kept = accounts::Account::new(
        accounts::AccountId::from(id.clone()),
        calendar_model::account::TYPE_ID,
    );
    ADDRESS.clone_into(&mut kept.display_name);
    kept.config = json!({
        "display_name": ADDRESS,
        "server_url": server_url,
        "username": ADDRESS,
        "calendar_provider": "google",
        "calendars": [{
            "id": server.calendar_url("work"),
            "name": "Work",
            "color": 0,
            "visible": true,
            "read_only": false,
        }],
    });
    crate::workspaces::person_settings::seal_grant_for_test(root, &kept, tokens)
        .expect("the grant is sealed");
    let mut store = accounts::AccountStore::load_in(root);
    store.upsert(kept);
    store.save_in(root).expect("the account is kept");
    id
}

/// Whether a sign-in is still kept for `account` in the workspace's vault.
fn secret_kept(root: &Path, account: &str) -> bool {
    crate::workspaces::person_settings::credentials(root)
        .expect("the workspace's vault")
        .get(&accounts::AccountId::from(account.to_owned()))
        .is_some()
}

/// A grant that has not expired is sent as it is: no renewal, and the
/// account lists as signed in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_grant_reads_the_calendar_server_with_its_access_token() {
    let fake = FakeAuthorizationServer::start();
    let mut env = ScopedEnv::new();
    web_client(&mut env, &fake);
    let ada = Ada::signing_in_as(ADDRESS)
        .await
        .reading_a_grant("access-token-1");
    let her = ada.herself();
    let root = ada.root();
    let id = seed_google_calendar(
        &root,
        &ada.server,
        &grant("access-token-1", "refresh-1", 3600),
    );
    ada.server.put(
        "work",
        "review.ics",
        &meeting("review-1@example.org", "Review", "20261117T100000", ""),
    );

    let listed = ada
        .ask(&her, held::SYNC, json!({"account": id}))
        .await
        .expect("the holder reads the server its grant opens");
    assert_eq!(listed["accounts"][0]["hasSecret"], true, "{listed}");
    assert_eq!(fake.refreshes(), 0, "a fresh token is sent as it is");
    assert_eq!(
        ada.server.bearer_tokens(),
        ["access-token-1".to_owned()],
        "every request carried the grant's access token"
    );
    assert_eq!(
        ada.week(&her).await,
        ["Review 2026-11-17T10:00:00+01:00"],
        "what the grant opens is kept and read with every device off"
    );
}

/// A grant that has expired is renewed through the web client the sign-in used,
/// and the server is read with the token that renewal gave.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_expired_grant_is_renewed_before_the_server_is_read() {
    let fake = FakeAuthorizationServer::start();
    fake.set_token_lifetime(3600);
    fake.issue_access_token("access-token-2");
    fake.accept_refresh_token("refresh-1");
    fake.require_client_secret("web-secret");
    let mut env = ScopedEnv::new();
    web_client(&mut env, &fake);
    let ada = Ada::signing_in_as(ADDRESS)
        .await
        .reading_a_grant("access-token-2");
    let her = ada.herself();
    let root = ada.root();
    // No lifetime reported: renewed before every read.
    let id = seed_google_calendar(
        &root,
        &ada.server,
        &grant("access-token-old", "refresh-1", 0),
    );
    ada.server.put(
        "work",
        "review.ics",
        &meeting("review-1@example.org", "Review", "20261117T100000", ""),
    );

    let listed = ada
        .ask(&her, held::SYNC, json!({"account": id}))
        .await
        .expect("the renewed grant reads the server");
    assert_eq!(listed["accounts"][0]["hasSecret"], true, "{listed}");
    assert_eq!(fake.refreshes(), 1, "the token was renewed once");
    let sent = ada.server.bearer_tokens();
    assert!(!sent.is_empty(), "the server was read at all");
    assert!(
        sent.iter().all(|token| token == "access-token-2"),
        "the renewed token was sent, not the old one: {sent:?}"
    );
    assert_eq!(
        ada.week(&her).await,
        ["Review 2026-11-17T10:00:00+01:00"],
        "the renewed grant reads the entries"
    );
}

/// A provider that cannot be reached and one that will not renew the grant
/// are answered differently: the first keeps the grant so a later try may
/// end, the second takes it out of the vault and says to sign in again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grant_the_provider_will_not_renew_leaves_the_account_needing_a_sign_in() {
    let fake = FakeAuthorizationServer::start();
    fake.set_token_lifetime(3600);
    fake.require_client_secret("web-secret");
    let mut env = ScopedEnv::new();
    web_client(&mut env, &fake);
    let ada = Ada::signing_in_as(ADDRESS)
        .await
        .reading_a_grant("access-token-2");
    let her = ada.herself();
    let root = ada.root();
    // The provider knows no such refresh token: it will never renew this
    // grant.
    let id = seed_google_calendar(
        &root,
        &ada.server,
        &grant("access-token-old", "refresh-dead", 0),
    );

    // The provider was not reached: that is not the person's sign-in ending.
    env.set("TILO_OAUTH_GOOGLE_TOKEN_URL", "http://127.0.0.1:1/token");
    let unreached = refused(
        "a renewal whose provider was not reached",
        ada.ask(&her, held::SYNC, json!({"account": id})).await,
    );
    assert!(
        unreached.contains("could not be reached"),
        "the refusal says the provider was not reached: {unreached}"
    );
    assert!(
        secret_kept(&root, &id),
        "a grant that was not reached stays in the vault"
    );
    let listed = ada
        .ask(&her, held::LIST, json!({}))
        .await
        .expect("the list reads");
    assert_eq!(
        listed["accounts"][0]["hasSecret"], true,
        "an account whose renewal was not reached still has its sign-in: {listed}"
    );

    // The provider refuses the grant for good: the account needs a sign-in,
    // and none is kept.
    env.set(
        "TILO_OAUTH_GOOGLE_TOKEN_URL",
        format!("{}/token", fake.url()),
    );
    let again = refused(
        "a grant the provider will not renew",
        ada.ask(&her, held::SYNC, json!({"account": id})).await,
    );
    assert_eq!(
        again, "That account's sign-in needs renewing. Sign in again",
        "the refusal is in plain words"
    );
    assert!(
        !secret_kept(&root, &id),
        "the dead grant is taken out of the vault"
    );
    let listed = ada
        .ask(&her, held::LIST, json!({}))
        .await
        .expect("the list reads");
    assert_eq!(
        listed["accounts"][0]["hasSecret"], false,
        "the account lists as one that needs a sign-in again: {listed}"
    );
    assert!(
        listed["accounts"][0]["problem"]
            .as_str()
            .is_some_and(|problem| problem.contains("Sign in again")),
        "the list says why: {listed}"
    );
}
