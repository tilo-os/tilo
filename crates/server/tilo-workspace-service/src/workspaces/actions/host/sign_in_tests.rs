//! Signing in on a provider's own page, from a window: begun, answered by a
//! stand-in provider, and kept as the person's own account.
//!
//! Every provider here is a stand-in on loopback: no real account, password
//! or mailbox is touched. What a browser would do is done by the test, which
//! follows the address the server answers and hands the provider's answer on
//! to the route the browser comes back to.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the server handle a test binds is kept until the test ends; dropping it earlier closes the service it drives"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::person_accounts::{self, MailAccounts, Provider, SignInStep};
use tilo_actions::remote::{ActionProvider as _, InvocationReceipt, ReceiptStatus};

use super::super::ServiceWorkspaceActions;
use super::super::tests::{caller, settled};
use super::accounts_tests::{asking, key_of};
use crate::workspaces::tests::{governor, test_host};

/// What an action answered.
fn answered<T: serde::de::DeserializeOwned>(receipt: InvocationReceipt) -> T {
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the server answers at once, not {:?}", receipt.status);
    };
    serde_json::from_value(output.value.expect("an answer")).expect("the answer reads")
}

/// Where a provider sends the browser back to: it keeps the one request of
/// that redirect, so a test can hand its answer on the way the server's own
/// route would.
struct Landing {
    origin: String,
    asked: Arc<Mutex<Option<String>>>,
}

impl Landing {
    fn stand() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a page to land on");
        let origin = format!("http://{}", listener.local_addr().expect("its address"));
        let asked = Arc::new(Mutex::new(None));
        let kept = Arc::clone(&asked);
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().expect("the page is readable"));
            let mut request = String::new();
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let ended = line == "\r\n" || line == "\n";
                if request.is_empty() {
                    request = line.clone();
                }
                line.clear();
                if ended {
                    break;
                }
            }
            *kept.lock().unwrap() = Some(request);
            let mut stream = stream;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        });
        Self { origin, asked }
    }

    /// The query the provider's redirect carried, once the browser landed.
    fn query(&self) -> String {
        let request = self
            .asked
            .lock()
            .unwrap()
            .clone()
            .expect("the browser landed");
        let target = request.split_whitespace().nth(1).unwrap_or_default();
        target
            .split_once('?')
            .map_or_else(String::new, |(_, query)| query.to_owned())
    }
}

/// The stand-in provider's endpoints, as the server reads them from its
/// environment.
fn web_client(
    env: &mut tilo_test_env::ScopedEnv,
    issuer: &str,
    fake: &tilo_oauth::fake::FakeAuthorizationServer,
) {
    let base = fake.url();
    env.set(format!("TILO_OAUTH_{issuer}_WEB_CLIENT_ID"), "web-client")
        .set(
            &format!("TILO_OAUTH_{issuer}_AUTHORIZE_URL"),
            format!("{base}/authorize"),
        )
        .set(
            &format!("TILO_OAUTH_{issuer}_TOKEN_URL"),
            format!("{base}/token"),
        )
        .set(
            &format!("TILO_OAUTH_{issuer}_PROFILE_URL"),
            fake.profile_url(),
        );
}

/// The account a signed-in person keeps, by its address.
fn sealed(root: &std::path::Path, address: &str) -> Option<accounts::Secret> {
    crate::workspaces::person_settings::credentials(root)
        .ok()
        .and_then(|vault| vault.get(&accounts::AccountId::from(address.to_owned())))
}

/// Wait until `ready`, which a background sign-in makes true.
async fn until(what: &str, ready: impl Fn() -> bool + Send) {
    let waited = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while !ready() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "{what}");
}

/// A person who signs in with Google gets the account, its sealed grant and
/// its calendar, and the same state cannot be handed in twice.
#[tokio::test]
async fn a_google_sign_in_keeps_the_account_and_its_calendar() {
    let fake = tilo_oauth::fake::FakeAuthorizationServer::start();
    fake.set_identity("ada@gmail.example");
    let mut env = tilo_test_env::ScopedEnv::new();
    web_client(&mut env, "GOOGLE", &fake);
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let actions = ServiceWorkspaceActions::new();
    actions.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    let device = key_of(&root, "studio");
    let person = root.verifying_key().to_bytes();
    let page = actions.in_workspace_as(
        workspaces.create("Mail").await.unwrap().workspace_id,
        &device,
    );
    let ada = caller(&device);
    let landing = Landing::stand();
    workspaces.accounts_call_back_at(landing.origin.clone());

    let step: SignInStep = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({"provider": "google", "services": {"mail": true, "calendar": true}}),
            ),
        ))
        .await
        .unwrap(),
    );
    assert!(step.open.starts_with(&fake.url()), "{}", step.open);

    // Until it finishes, the page shows it running, and Google's button is
    // one the server offers.
    let running: MailAccounts = answered(
        settled(page.invoke(&ada, asking(person_accounts::LIST, json!({}))))
            .await
            .unwrap(),
    );
    assert_eq!(running.signing_in.len(), 1);
    assert_eq!(running.signing_in[0].provider, Provider::Google);
    assert!(running.signs_in_with.contains(&Provider::Google));

    // The browser opens the provider's page and comes back.
    tilo_oauth::fake::browse(&step.open).unwrap();
    workspaces.accounts_return(&landing.query()).await.unwrap();

    let listed: MailAccounts = answered(
        settled(page.invoke(&ada, asking(person_accounts::LIST, json!({}))))
            .await
            .unwrap(),
    );
    assert!(listed.signing_in.is_empty(), "{listed:?}");
    assert!(
        listed
            .accounts
            .iter()
            .any(|account| account.address == "ada@gmail.example"),
        "{listed:?}"
    );
    // A reading never carries the grant.
    assert!(!serde_json::to_string(&listed).unwrap().contains("access-"));

    let keeping = workspaces.person_settings_root(&person);
    let grant = sealed(&keeping, "ada@gmail.example").expect("the grant is sealed");
    assert!(grant.token.is_some() && grant.password.is_empty());
    // The same grant opens the calendar server the account feeds.
    let calendar = accounts::AccountStore::load_in(&keeping)
        .all()
        .iter()
        .find(|account| account.type_id.as_str() == "caldav")
        .cloned()
        .expect("the calendar server is kept");
    assert_eq!(
        calendar
            .config
            .get("calendar_provider")
            .and_then(Value::as_str),
        Some("google")
    );
    assert!(sealed(&keeping, calendar.id.as_str()).is_some_and(|secret| secret.token.is_some()));

    // The state was used up: the same answer finishes nothing twice.
    assert!(workspaces.accounts_return(&landing.query()).await.is_err());
}

/// Microsoft signs in the same way, and keeps the mailbox the grant names.
#[tokio::test]
async fn a_microsoft_sign_in_keeps_the_account_too() {
    let fake = tilo_oauth::fake::FakeAuthorizationServer::start();
    fake.set_identity("ada@outlook.example");
    let mut env = tilo_test_env::ScopedEnv::new();
    web_client(&mut env, "MICROSOFT", &fake);
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let actions = ServiceWorkspaceActions::new();
    actions.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    let device = key_of(&root, "laptop");
    let person = root.verifying_key().to_bytes();
    let page = actions.in_workspace_as(
        workspaces.create("Mail").await.unwrap().workspace_id,
        &device,
    );
    let ada = caller(&device);
    let landing = Landing::stand();
    workspaces.accounts_call_back_at(landing.origin.clone());

    let step: SignInStep = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({"provider": "microsoft"}),
            ),
        ))
        .await
        .unwrap(),
    );
    tilo_oauth::fake::browse(&step.open).unwrap();
    workspaces.accounts_return(&landing.query()).await.unwrap();

    let listed: MailAccounts = answered(
        settled(page.invoke(&ada, asking(person_accounts::LIST, json!({}))))
            .await
            .unwrap(),
    );
    assert!(
        listed
            .accounts
            .iter()
            .any(|account| account.address == "ada@outlook.example"),
        "{listed:?}"
    );
    let keeping = workspaces.person_settings_root(&person);
    let account = accounts::AccountStore::load_in(&keeping)
        .get(&accounts::AccountId::from("ada@outlook.example".to_owned()))
        .cloned()
        .expect("the account is kept");
    assert_eq!(
        account.config.get("provider").and_then(Value::as_str),
        Some("microsoft")
    );
}

/// One person's sign-ins are neither shown to nor finished by another, and
/// one that ran out finishes nothing.
#[tokio::test]
async fn a_sign_in_is_the_person_who_began_it_and_goes_stale() {
    let fake = tilo_oauth::fake::FakeAuthorizationServer::start();
    let mut env = tilo_test_env::ScopedEnv::new();
    web_client(&mut env, "GOOGLE", &fake);
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let actions = ServiceWorkspaceActions::new();
    actions.attach(workspaces.clone());
    let ada = SigningKey::from_bytes(&rand::random());
    let ada_device = key_of(&ada, "studio");
    let ada_page = actions.in_workspace_as(
        workspaces.create("Mail").await.unwrap().workspace_id,
        &ada_device,
    );
    let landing = Landing::stand();
    workspaces.accounts_call_back_at(landing.origin.clone());
    let step: SignInStep = answered(
        settled(ada_page.invoke(
            &caller(&ada_device),
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({"provider": "google"}),
            ),
        ))
        .await
        .unwrap(),
    );

    // Somebody else's window shows none of it, and can stop none of it.
    let bob = SigningKey::from_bytes(&rand::random());
    let bob_device = key_of(&bob, "laptop");
    let bob_page = actions.in_workspace_as(
        workspaces.create("Other").await.unwrap().workspace_id,
        &bob_device,
    );
    let theirs: MailAccounts = answered(
        settled(bob_page.invoke(
            &caller(&bob_device),
            asking(person_accounts::LIST, json!({})),
        ))
        .await
        .unwrap(),
    );
    assert!(theirs.signing_in.is_empty());
    assert!(
        settled(bob_page.invoke(
            &caller(&bob_device),
            asking(person_accounts::CANCEL_SIGN_IN, json!({"signIn": step.id}),),
        ))
        .await
        .is_err()
    );

    // Gone stale, it finishes nothing.
    crate::workspaces::person_settings::expire_sign_in(&step.id);
    assert!(step.open.starts_with(&fake.url()), "{}", step.open);
    tilo_oauth::fake::browse(&step.open).unwrap();
    assert!(workspaces.accounts_return(&landing.query()).await.is_err());

    // A failure is kept with it, in words the person reads, until cancelled.
    let another: SignInStep = answered(
        settled(ada_page.invoke(
            &caller(&ada_device),
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({"provider": "google"}),
            ),
        ))
        .await
        .unwrap(),
    );
    let refused = workspaces
        .accounts_return(&format!("code=none&state=not-ours-{}", another.id))
        .await;
    assert!(refused.is_err());
    let stopped: MailAccounts = answered(
        settled(ada_page.invoke(
            &caller(&ada_device),
            asking(
                person_accounts::CANCEL_SIGN_IN,
                json!({"signIn": another.id}),
            ),
        ))
        .await
        .unwrap(),
    );
    assert!(stopped.signing_in.is_empty());
}

/// A Nextcloud server's login flow keeps the account, its app password and
/// the calendar server of the same sign-in; cancelling stops waiting.
/// A stand-in Nextcloud server that starts a login flow, and answers its
/// poll with a minted app password while `completes` says so.
fn nextcloud_server(
    completes: Arc<std::sync::atomic::AtomicBool>,
) -> (test_http::Responder, Arc<Mutex<String>>) {
    use test_http::{Reply, Responder};

    let base = Arc::new(Mutex::new(String::new()));
    let for_handler = Arc::clone(&base);
    let answering = Arc::clone(&completes);
    let responder = Responder::handle(move |request| {
        let base = for_handler.lock().unwrap().clone();
        match (request.method.as_str(), request.target.as_str()) {
            ("POST", "/index.php/login/v2") => Some(Reply::status(200).body(
                serde_json::json!({
                    "poll": {"token": "poll-token", "endpoint": format!("{base}/login/v2/poll")},
                    "login": format!("{base}/login/v2/flow/abc"),
                })
                .to_string(),
            )),
            ("POST", "/login/v2/poll") => Some(if answering.load(std::sync::atomic::Ordering::SeqCst)
            {
                Reply::status(200).body(
                    serde_json::json!({
                        "server": base,
                        "loginName": "ada",
                        "appPassword": "minted-app-password",
                    })
                    .to_string(),
                )
            } else {
                Reply::status(404)
            }),
            _ => Some(Reply::status(404)),
        }
    });
    *base.lock().unwrap() = responder.url();
    (responder, base)
}

#[tokio::test]
async fn a_nextcloud_login_flow_keeps_the_account_and_its_calendar() {
    let completes = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (responder, _base) = nextcloud_server(completes);

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces.call_private_model_addresses(true);
    let actions = ServiceWorkspaceActions::new();
    actions.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    let device = key_of(&root, "studio");
    let person = root.verifying_key().to_bytes();
    let page = actions.in_workspace_as(
        workspaces.create("Mail").await.unwrap().workspace_id,
        &device,
    );
    let ada = caller(&device);

    let step: SignInStep = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({"provider": "nextcloud", "server": responder.url()}),
            ),
        ))
        .await
        .unwrap(),
    );
    assert_eq!(step.open, format!("{}/login/v2/flow/abc", responder.url()));
    let keeping = workspaces.person_settings_root(&person);
    until("the Nextcloud account is kept", || {
        accounts::AccountStore::load_in(&keeping)
            .all()
            .iter()
            .any(|account| account.type_id.as_str() == accounts::cloud_types::NEXTCLOUD)
    })
    .await;
    let app_password = crate::workspaces::person_settings::credentials(&keeping)
        .unwrap()
        .get(&accounts::AccountId::from(
            "nextcloud:ada@127.0.0.1".to_owned(),
        ))
        .map_or_else(String::new, |secret| secret.password);
    assert_eq!(app_password, "minted-app-password");
    let calendar = accounts::AccountStore::load_in(&keeping)
        .all()
        .iter()
        .find(|account| account.type_id.as_str() == "caldav")
        .cloned()
        .expect("the calendar server is kept");
    assert_eq!(
        calendar
            .config
            .get("calendar_provider")
            .and_then(Value::as_str),
        Some("nextcloud")
    );

    // A flow nobody approves waits, and cancelling stops it.
    completes.store(false, std::sync::atomic::Ordering::SeqCst);
    let waiting: SignInStep = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({"provider": "nextcloud", "server": responder.url()}),
            ),
        ))
        .await
        .unwrap(),
    );
    let listed: MailAccounts = answered(
        settled(page.invoke(&ada, asking(person_accounts::LIST, json!({}))))
            .await
            .unwrap(),
    );
    assert_eq!(listed.signing_in.len(), 1);
    let stopped: MailAccounts = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::CANCEL_SIGN_IN,
                json!({"signIn": waiting.id}),
            ),
        ))
        .await
        .unwrap(),
    );
    assert!(stopped.signing_in.is_empty());
}
