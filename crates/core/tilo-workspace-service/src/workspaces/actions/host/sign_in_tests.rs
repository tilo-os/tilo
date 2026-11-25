//! Signing in on a provider's own page, from a window: a Nextcloud server's
//! login flow, begun, answered by a stand-in server, and kept as the
//! workspace's account.
//!
//! The server is a stand-in on loopback: no real account, password or
//! mailbox is touched.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_actions::remote::person_accounts::{self, MailAccounts, SignInStep};
use tilo_actions::remote::{ActionProvider as _, InvocationReceipt, ReceiptStatus};

use super::super::tests::{admit, caller, governed, settled};
use super::accounts_tests::asking;
use tilo_server_protocol::WorkspaceId;

use crate::workspaces::Workspaces;
use crate::workspaces::tests::{governor, test_host};

/// What an action answered.
fn answered<T: serde::de::DeserializeOwned>(receipt: InvocationReceipt) -> T {
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the server answers at once, not {:?}", receipt.status);
    };
    serde_json::from_value(output.value.expect("an answer")).expect("the answer reads")
}

/// The account of `workspace` of the kind `type_id`, once the workspace's accounts keep one.
async fn kept_of_type(
    workspaces: &Workspaces,
    workspace: WorkspaceId,
    type_id: &str,
) -> Option<accounts::Account> {
    workspaces
        .workspace_accounts(&workspace.to_string())
        .await
        .ok()?
        .of_type(type_id)
        .into_iter()
        .next()
}

/// The account of `workspace` of the kind `type_id`, once the background poll kept it.
async fn kept_soon(workspaces: &Workspaces, workspace: WorkspaceId, type_id: &str) {
    let kept = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while kept_of_type(workspaces, workspace, type_id).await.is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(kept.is_ok(), "the {type_id} account is kept");
}

/// The sign-in the account `id` of a signed-in workspace names: what the entry of the
/// workspace's vault it names holds.
async fn sealed(workspaces: &Workspaces, workspace: WorkspaceId, id: &str) -> Option<String> {
    let workspace = workspace.to_string();
    let named = workspaces
        .workspace_accounts(&workspace)
        .await
        .ok()?
        .account(id)
        .and_then(|kept| {
            accounts::email::password_item(&kept.config)
                .and_then(tilo_actions::remote::person_vault::ItemRef::parse)
        })?;
    let vault = workspaces.open_vault(&workspace).await.ok()?;
    vault.entry(named.id()).map(|entry| entry.password.clone())
}

/// A stand-in Nextcloud server that starts a login flow, and answers its
/// poll with a minted app password while `completes` says so.
fn nextcloud_server(
    completes: &Arc<std::sync::atomic::AtomicBool>,
) -> (test_http::Responder, Arc<Mutex<String>>) {
    use test_http::{Reply, Responder};

    let base = Arc::new(Mutex::new(String::new()));
    let for_handler = Arc::clone(&base);
    let answering = Arc::clone(completes);
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

/// A Nextcloud server's login flow keeps the account, its app password and
/// the calendar server of the same sign-in; a flow begun is its workspace's
/// alone, and cancelling stops waiting.
#[tokio::test]
async fn a_nextcloud_login_flow_keeps_the_account_and_its_calendar() {
    let completes = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (responder, _base) = nextcloud_server(&completes);

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces.call_private_model_addresses(true);
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.device_page(workspace_id, &device);
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
    kept_soon(workspaces, workspace_id, accounts::cloud_types::NEXTCLOUD).await;
    let app_password = sealed(workspaces, workspace_id, "nextcloud:ada@127.0.0.1")
        .await
        .unwrap_or_default();
    assert_eq!(app_password, "minted-app-password");
    kept_soon(workspaces, workspace_id, "caldav").await;
    let calendar = kept_of_type(workspaces, workspace_id, "caldav")
        .await
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

    // Another workspace's member shows none of it, and can stop none of it.
    let (elsewhere, elsewhere_id, _) = governed(workspaces).await;
    let bob_device = admit(workspaces, &elsewhere, &owner, 2).await;
    let bob_page = actions.device_page(elsewhere_id, &bob_device);
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
            asking(
                person_accounts::CANCEL_SIGN_IN,
                json!({"signIn": waiting.id})
            ),
        ))
        .await
        .is_err()
    );
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

/// Under the default public-only reach, beginning a Nextcloud sign-in to an
/// address of this machine or its network is refused before anything is sent.
#[tokio::test]
async fn a_nextcloud_sign_in_to_an_inside_address_is_refused_and_touches_nobody() {
    let silent = test_http::Responder::silent();

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.device_page(workspace_id, &device);
    let ada = caller(&device);

    for inside in [
        silent.url(),
        format!("http://localhost:{}", silent.port()),
        "https://[::1]".into(),
        "https://10.0.0.7".into(),
        "https://2130706433".into(),
    ] {
        let refused = settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({"provider": "nextcloud", "server": inside}),
            ),
        ))
        .await
        .unwrap_err();
        assert_eq!(
            refused,
            tilo_actions::remote::ActionCallError::Refused {
                reason: "That server's address is not one this server may call".into(),
            },
            "{inside}"
        );
    }
    silent.assert_untouched();
}

/// A stand-in Apple sign-in: it accepts one made-up `Apple ID` and password,
/// asks for the code Apple sends, and takes one made-up code.
struct StandInApple;

impl account_credentials::icloud::ICloudAuth for StandInApple {
    fn sign_in(
        &self,
        apple_id: &str,
        password: &str,
    ) -> Result<
        account_credentials::icloud::ICloudSignIn,
        account_credentials::icloud::ICloudAuthError,
    > {
        use account_credentials::icloud::{ICloudAuthError, ICloudChallenge, ICloudSignIn};
        if apple_id.trim().is_empty() || password.is_empty() {
            return Err(ICloudAuthError::InvalidCredentials);
        }
        Ok(ICloudSignIn::NeedsCode(ICloudChallenge(
            zeroize::Zeroizing::new("standing-in".to_owned()),
        )))
    }

    fn verify_code(
        &self,
        _challenge: &account_credentials::icloud::ICloudChallenge,
        code: &str,
    ) -> Result<
        account_credentials::icloud::ICloudSessionState,
        account_credentials::icloud::ICloudAuthError,
    > {
        use account_credentials::icloud::{ICloudAuthError, ICloudSessionState};
        if code.trim() != "123456" {
            return Err(ICloudAuthError::InvalidCode);
        }
        Ok(ICloudSessionState(zeroize::Zeroizing::new(
            "apple-web-session".to_owned(),
        )))
    }

    fn resume(
        &self,
        _session: &account_credentials::icloud::ICloudSessionState,
    ) -> Result<
        (
            account_credentials::icloud::ICloudSessionState,
            account_credentials::icloud::ICloudHeaders,
        ),
        account_credentials::icloud::ICloudAuthError,
    > {
        Err(account_credentials::icloud::ICloudAuthError::Unavailable)
    }
}

/// The accounts `workspace` keeps, as its apps read them.
async fn kept_reading(workspaces: &Workspaces, workspace: &WorkspaceId) -> Value {
    workspaces.accounts_reading(&workspace.to_string()).await
}

/// An `Apple ID`'s sign-in keeps no account until the code Apple sent is
/// entered, the wrong code is refused in words, and the right one keeps the
/// drive with its session in the workspace's vault.
#[tokio::test]
async fn an_icloud_drive_waits_for_the_code_apple_sends() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.device_page(workspace_id, &device);
    let ada = caller(&device);
    workspaces.icloud_sign_in_through(std::sync::Arc::new(StandInApple));

    let step: SignInStep = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_CLOUD_SIGN_IN,
                json!({
                    "kind": "icloud-drive",
                    "name": "My iCloud",
                    "username": "ada@icloud.example",
                    "password": "made-up-password",
                }),
            ),
        ))
        .await
        .unwrap(),
    );
    assert!(!step.id.is_empty(), "the sign-in is named");
    assert!(
        step.code.contains("verification code"),
        "the step says what it waits for: {}",
        step.code
    );

    // Nothing is kept while no code was entered.
    let reading = kept_reading(workspaces, &workspace_id).await;
    assert_eq!(reading["accounts"], json!([]), "{reading}");

    // A wrong code is refused and the sign-in stays open.
    let wrong = settled(page.invoke(
        &ada,
        asking(
            person_accounts::SUBMIT_SIGN_IN_CODE,
            json!({"signIn": step.id, "code": "000000"}),
        ),
    ))
    .await
    .unwrap_err();
    assert!(
        matches!(&wrong, tilo_actions::remote::ActionCallError::Refused { reason } if reason.contains("verification code")),
        "{wrong:?}"
    );

    let submitted = settled(page.invoke(
        &ada,
        asking(
            person_accounts::SUBMIT_SIGN_IN_CODE,
            json!({"signIn": step.id, "code": "123456"}),
        ),
    ))
    .await;
    assert!(submitted.is_ok(), "{submitted:?}");

    let reading = kept_reading(workspaces, &workspace_id).await;
    let account = reading["accounts"]
        .as_array()
        .and_then(|accounts| accounts.first())
        .expect("her drive reads");
    assert_eq!(account["kind"], json!("cloud"), "{reading}");
    assert_eq!(account["name"], json!("My iCloud"), "{reading}");
    assert_eq!(
        account["services"],
        json!({"mail": false, "calendar": false, "files": true}),
        "its files are what it feeds"
    );

    // Its session is the workspace's vault's, not read back anywhere.
    let id = account["id"].as_str().expect("an id").to_owned();
    let kept = workspaces
        .workspace_accounts(&workspace_id.to_string())
        .await
        .unwrap()
        .account(&id)
        .map(|account| account.config)
        .expect("the drive is kept");
    assert_eq!(kept["identity"], json!("ada@icloud.example"));
    let reference = accounts::email::password_item(&kept).expect("its sign-in is named");
    let entry = tilo_actions::remote::person_vault::ItemRef::parse(reference)
        .expect("a reference to a vault item")
        .id()
        .to_owned();
    let sealed = workspaces
        .open_vault(&workspace_id.to_string())
        .await
        .unwrap();
    let secret = sealed
        .entries()
        .iter()
        .find(|(kept_entry, _)| kept_entry.id.0 == entry)
        .map(|(entry, _)| entry.password.clone());
    assert!(
        secret.is_some_and(|secret| secret.contains("apple-web-session")),
        "the session is sealed in the workspace's vault"
    );
}

/// A drive is kept once its provider's own page approved it: the sign-in
/// answers the page, the browser approves there, and the account arrives
/// with its mount.
#[tokio::test]
async fn a_drive_is_kept_once_its_provider_page_approves_it() {
    use account_credentials::{CredentialStore, OAuthCloud};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.device_page(workspace_id, &device);
    let ada = caller(&device);

    let provider = tilo_oauth::fake::FakeAuthorizationServer::start();
    workspaces.cloud_sign_in_through(CredentialStore::for_root(None).with_provider(
        OAuthCloud::GoogleDrive,
        provider.provider(),
        "test-client",
    ));

    let step: SignInStep = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_CLOUD_SIGN_IN,
                json!({"kind": "google-drive", "name": "My Drive"}),
            ),
        ))
        .await
        .unwrap(),
    );
    assert!(!step.id.is_empty());
    assert!(step.code.is_empty(), "the loopback waits for the browser");
    let open = step.open.clone();

    // The person's browser approves the sign-in on the provider's page.
    tokio::task::spawn_blocking(move || tilo_oauth::fake::browse(&open))
        .await
        .unwrap()
        .expect("the browser approves the sign-in");
    kept_soon(
        workspaces,
        workspace_id,
        accounts::cloud_types::GOOGLE_DRIVE,
    )
    .await;

    let reading = kept_reading(workspaces, &workspace_id).await;
    let account = reading["accounts"]
        .as_array()
        .and_then(|accounts| accounts.first())
        .expect("the drive reads");
    assert_eq!(account["kind"], json!("cloud"), "{reading}");
    assert_eq!(account["name"], json!("My Drive"));
    assert_eq!(
        account["services"],
        json!({"mail": false, "calendar": false, "files": true})
    );
}

/// A `WebDAV` server is connected from the address and sign-in the person
/// typed, with no browser, and its files are what the account feeds.
#[tokio::test]
async fn a_webdav_server_is_kept_from_what_its_person_typed() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.device_page(workspace_id, &device);
    let ada = caller(&device);

    let step: SignInStep = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_CLOUD_SIGN_IN,
                json!({
                    "kind": "webdav",
                    "name": "Work files",
                    "server": "https://dav.example.test/files/ada",
                    "username": "ada",
                    "password": "made-up-password",
                }),
            ),
        ))
        .await
        .unwrap(),
    );
    assert!(
        step.id.is_empty(),
        "nothing waits for a browser: {}",
        step.open
    );

    let reading = kept_reading(workspaces, &workspace_id).await;
    let account = reading["accounts"]
        .as_array()
        .and_then(|accounts| accounts.first())
        .expect("the server reads");
    assert_eq!(account["kind"], json!("cloud"), "{reading}");
    assert_eq!(account["name"], json!("Work files"));
    assert_eq!(
        account["services"],
        json!({"mail": false, "calendar": false, "files": true})
    );

    // A server that names no address is refused before anything is kept.
    let refused = settled(page.invoke(
        &ada,
        asking(
            person_accounts::BEGIN_CLOUD_SIGN_IN,
            json!({"kind": "webdav", "server": "not an address", "username": "ada", "password": "x"}),
        ),
    ))
    .await
    .unwrap_err();
    assert!(
        matches!(&refused, tilo_actions::remote::ActionCallError::Refused { reason } if reason.contains("server address")),
        "{refused:?}"
    );
}

/// A mailbox is kept once its provider's own page approved it: Google says which mailbox the
/// grant is for, and the account arrives with that address and its mail, its grant sealed in
/// the workspace's vault as the sign-in Mail reads with.
#[tokio::test]
async fn a_mail_account_is_kept_once_google_s_page_approves_it() {
    use account_credentials::{CredentialStore, OAuthIssuer, OAuthUse};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.device_page(workspace_id, &device);
    let ada = caller(&device);

    let provider = tilo_oauth::fake::FakeAuthorizationServer::start();
    provider.set_identity("ada@gmail.example");
    workspaces.cloud_sign_in_through(
        CredentialStore::for_root(None)
            .with_use(
                OAuthUse::mail(OAuthIssuer::Google),
                provider.provider(),
                "test-client",
            )
            .with_gmail_profile_endpoint(provider.profile_url()),
    );
    workspaces.sign_in_accounts_through(|_address, _password| Ok("signed in".into()));

    let step: SignInStep = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({"provider": "google", "address": "ada@gmail.example", "mail": true}),
            ),
        ))
        .await
        .unwrap(),
    );
    assert!(!step.id.is_empty());
    assert!(step.code.is_empty(), "the loopback waits for the browser");
    // The sign-in goes to the provider's own page.
    assert!(step.open.starts_with(&provider.url()), "{}", step.open);

    tokio::task::spawn_blocking(move || tilo_oauth::fake::browse(&step.open))
        .await
        .unwrap()
        .expect("the browser approves the sign-in");
    kept_soon(workspaces, workspace_id, accounts::email::TYPE_ID).await;

    let reading = kept_reading(workspaces, &workspace_id).await;
    let account = reading["accounts"]
        .as_array()
        .and_then(|accounts| accounts.first())
        .expect("the mailbox reads");
    assert_eq!(account["kind"], json!("mail"), "{reading}");
    assert_eq!(account["address"], json!("ada@gmail.example"));
    assert_eq!(account["provider"], json!("google"));
    assert_eq!(
        account["services"],
        json!({"mail": true, "calendar": false, "files": false})
    );

    // The vault keeps the grant as the account's own sign-in, and no secret is answered back.
    let config = workspaces
        .workspace_accounts(&workspace_id.to_string())
        .await
        .unwrap()
        .account("ada@gmail.example")
        .map(|account| account.config)
        .expect("the account is kept");
    assert!(
        accounts::email::is_oauth(&config),
        "the account signs in with a token: {config}"
    );
    let sealed = sealed(workspaces, workspace_id, "ada@gmail.example")
        .await
        .expect("the vault keeps its sign-in");
    assert!(!sealed.is_empty());
    assert!(!reading.to_string().contains(&sealed), "{reading}");
}

/// Microsoft's mail scopes read no profile, so the address its person typed is the one kept,
/// proved by signing in at its servers with the grant.
#[tokio::test]
async fn a_microsoft_mail_account_keeps_the_address_its_person_typed() {
    use account_credentials::{CredentialStore, OAuthIssuer, OAuthUse};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, actions) = governed(workspaces).await;
    let device = admit(workspaces, &workspace, &owner, 1).await;
    let page = actions.device_page(workspace_id, &device);
    let ada = caller(&device);

    // The address is proved at the servers the grant is for: a stand-in takes the token.
    let proved = std::sync::Arc::new(Mutex::new(Vec::new()));
    workspaces.sign_in_accounts_through({
        let proved = std::sync::Arc::clone(&proved);
        move |account, token| {
            proved
                .lock()
                .unwrap()
                .push((account.to_owned(), token.to_owned()));
            Ok("signed in".into())
        }
    });
    let provider = tilo_oauth::fake::FakeAuthorizationServer::start();
    workspaces.cloud_sign_in_through(CredentialStore::for_root(None).with_use(
        OAuthUse::mail(OAuthIssuer::Microsoft),
        provider.provider(),
        "test-client",
    ));

    // An address left out is refused in words its person reads, before anything is begun.
    let refused = settled(page.invoke(
        &ada,
        asking(
            person_accounts::BEGIN_SIGN_IN,
            json!({"provider": "microsoft", "mail": true}),
        ),
    ))
    .await
    .unwrap_err();
    assert!(
        matches!(&refused, tilo_actions::remote::ActionCallError::Refused { reason } if reason.contains("Microsoft")),
        "{refused:?}"
    );

    let step: SignInStep = answered(
        settled(page.invoke(
            &ada,
            asking(
                person_accounts::BEGIN_SIGN_IN,
                json!({
                    "provider": "microsoft",
                    "address": "ada@outlook.example",
                    "mail": true,
                }),
            ),
        ))
        .await
        .unwrap(),
    );
    tokio::task::spawn_blocking(move || tilo_oauth::fake::browse(&step.open))
        .await
        .unwrap()
        .expect("the browser approves the sign-in");
    kept_soon(workspaces, workspace_id, accounts::email::TYPE_ID).await;

    let proved = proved.lock().unwrap().clone();
    assert_eq!(proved.len(), 1, "{proved:?}");
    assert_eq!(proved[0].0, "ada@outlook.example");
    let reading = kept_reading(workspaces, &workspace_id).await;
    let account = reading["accounts"]
        .as_array()
        .and_then(|accounts| accounts.first())
        .expect("the mailbox reads");
    assert_eq!(account["address"], json!("ada@outlook.example"));
    assert_eq!(account["provider"], json!("microsoft"));
    // A server at its usual port is named without one.
    assert_eq!(account["incoming"], json!("outlook.office365.com"));
    assert_eq!(account["outgoing"], json!("smtp.office365.com:587"));
}
