//! A person with only a browser open manages what the workspace's server keeps: its model
//! providers, its mail accounts and its vault. No other device of theirs is online.
//!
//! Every ask takes the path a Settings window on a page takes: the window asks the desktop's
//! session, which hands it to the workspace's server over the page's own session with it. The
//! server stores and answers; nothing waits for a laptop or any other device of the person's.
use super::hosted_secret_use::Hosted;
use super::*;
use std::sync::Mutex;
use tilo_actions::remote::{
    ActionProvider as _, Caller, ReceiptStatus, person_accounts, person_models, person_vault,
};
use tilo_desktop_surface::session::DesktopSession;
use tilo_session_actions::SessionActions;

/// A made-up key: no provider ever issued it.
const KEY: &str = "made-up-key-of-a-page-only-person-0123456789";
/// What the stand-in for her mail servers takes as her password. Made up for this file.
const PASSWORD: &str = "only-her-mail-servers-may-read-this";
/// A made-up sign-in for a site, kept in the workspace's vault.
const SITE_SECRET: &str = "made-up-site-password-for-a-vault-entry";

/// The desktop session a page serves its windows, carrying what the workspace's server keeps to
/// that server over the page's own session, as a page's desktop does.
fn page_desktop(hosted: &Hosted) -> Arc<DesktopSession> {
    let (session, _openings) =
        tilo_desktop_surface::session::serve(hosted.workspace.clone(), Arc::default());
    let page = &hosted.page;
    let server = SessionActions::new(&page.peer, page.workspace, page.host_key);
    session.serve_workspace_api(Arc::new(server));
    session
}

/// One ask of a Settings window, as the person, followed to its answer.
async fn asks(
    session: &DesktopSession,
    caller: &Caller,
    target: RemoteActionTarget,
    argument: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let argument = match argument {
        serde_json::Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    let invocation = InvocationId::new();
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation,
        target,
        argument,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    };
    let mut receipt = tilo_test_env::within("the server answers", session.invoke(caller, request))
        .await
        .expect("the session answers")
        .map_err(|error| error.to_string())?;
    let deadline = tilo_test_env::hang_deadline();
    while matches!(receipt.status, ReceiptStatus::Accepted) {
        assert!(Instant::now() < deadline, "never settled: {receipt:?}");
        tokio::time::sleep(Duration::from_millis(25)).await;
        if let Some(state) = session.observe(caller, &invocation) {
            receipt = state.receipt();
        }
    }
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        other => Err(format!("{other:?}")),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_with_only_a_page_manages_models_mail_and_vault_on_the_host() {
    let hosted = Hosted::start().await;
    let proved = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&proved);
    hosted
        .workspaces
        .sign_in_accounts_through(move |account, password| {
            if password == PASSWORD {
                kept.lock().unwrap().push(account.to_owned());
                Ok("signed in".into())
            } else {
                Err("IMAP server rejected the saved credential".into())
            }
        });
    let session = page_desktop(&hosted);
    let person = hex::encode(hosted.root.verifying_key().to_bytes());
    let me = Caller::local_person(person).in_workspace(hosted.workspace.clone());
    let models = |action| person_models::target(action);
    let mail = |action| person_accounts::target(action);
    let vault = |action| person_vault::target(action);

    // A model provider: added, its key and a model set, then removed.
    let added = asks(
        &session,
        &me,
        models(person_models::ADD_PROVIDER),
        serde_json::json!({
            "name": "Made-up models",
            "protocol": "open-ai-compatible",
            "endpoint": "https://models.example.com/v1",
        }),
    )
    .await
    .expect("the server adds the provider");
    let provider = added["id"].as_str().expect("its id").to_owned();
    for (action, argument) in [
        (
            person_models::SET_KEY,
            serde_json::json!({"provider": provider, "key": KEY}),
        ),
        (
            person_models::ADD_MODEL,
            serde_json::json!({"provider": provider, "model": "made-up-main"}),
        ),
    ] {
        asks(&session, &me, models(action), argument)
            .await
            .unwrap_or_else(|why| panic!("{action}: {why}"));
    }
    let listed = asks(
        &session,
        &me,
        models(person_models::LIST),
        serde_json::Value::Null,
    )
    .await
    .expect("the server lists the providers");
    let ours = listed["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|kept| kept["id"] == provider.as_str())
        .expect("the provider is kept")
        .clone();
    assert_eq!(ours["configured"], true, "its key is kept: {ours}");
    assert!(ours.to_string().contains("made-up-main"), "{ours}");
    assert!(!listed.to_string().contains(KEY), "no key is read back");
    asks(
        &session,
        &me,
        models(person_models::REMOVE_PROVIDER),
        serde_json::json!({"provider": provider}),
    )
    .await
    .expect("the server removes it");
    let after = asks(
        &session,
        &me,
        models(person_models::LIST),
        serde_json::Value::Null,
    )
    .await
    .unwrap();
    assert!(!after.to_string().contains(&provider), "removed: {after}");

    // A mail account: added with its sign-in, renamed, then removed.
    let account = |name: &str| {
        serde_json::json!({
            "address": "ada@example.test",
            "displayName": name,
            "incomingHost": "imap.provider.test",
            "outgoingHost": "smtp.provider.test",
            "password": PASSWORD,
        })
    };
    let added = asks(
        &session,
        &me,
        mail(person_accounts::ADD_MAIL),
        account("Ada"),
    )
    .await
    .expect("the server signs in and keeps the account");
    assert_eq!(added["hasSecret"], true, "{added}");
    assert_eq!(proved.lock().unwrap().len(), 1, "signed in once");
    asks(
        &session,
        &me,
        mail(person_accounts::ADD_MAIL),
        account("Ada at work"),
    )
    .await
    .expect("the server keeps the edit");
    let listed = asks(
        &session,
        &me,
        mail(person_accounts::LIST),
        serde_json::Value::Null,
    )
    .await
    .unwrap();
    let accounts = listed["accounts"].as_array().expect("accounts");
    assert_eq!(accounts.len(), 1, "edited in place: {listed}");
    assert_eq!(accounts[0]["displayName"], "Ada at work", "{listed}");
    assert!(
        !listed.to_string().contains(PASSWORD),
        "no password read back"
    );
    let id = accounts[0]["id"].as_str().expect("its id").to_owned();
    asks(
        &session,
        &me,
        mail(person_accounts::REMOVE),
        serde_json::json!({"account": id}),
    )
    .await
    .expect("the server removes it");
    let after = asks(
        &session,
        &me,
        mail(person_accounts::LIST),
        serde_json::Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(after["accounts"], serde_json::json!([]), "{after}");

    // A vault entry: kept, replaced by a new one, then removed.
    let entry = |secret: &str| {
        serde_json::json!({
            "kind": "login",
            "belongsTo": ["https://example.com"],
            "username": "ada",
            "secret": secret,
        })
    };
    let first = asks(&session, &me, vault(person_vault::ADD), entry(SITE_SECRET))
        .await
        .expect("the server keeps the entry");
    let first = first["reference"].as_str().expect("a reference").to_owned();
    let second = asks(
        &session,
        &me,
        vault(person_vault::ADD),
        entry("a-newer-one"),
    )
    .await
    .expect("the server keeps the new one");
    let second = second["reference"]
        .as_str()
        .expect("a reference")
        .to_owned();
    asks(
        &session,
        &me,
        vault(person_vault::REMOVE),
        serde_json::json!({"item": first}),
    )
    .await
    .expect("the old one goes");
    let listed = asks(
        &session,
        &me,
        vault(person_vault::LIST),
        serde_json::Value::Null,
    )
    .await
    .unwrap();
    let references: Vec<_> = listed["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["reference"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(references.contains(&second), "{listed}");
    assert!(!references.contains(&first), "{listed}");
    assert!(
        !listed.to_string().contains(SITE_SECRET),
        "nothing read back"
    );
    asks(
        &session,
        &me,
        vault(person_vault::REMOVE),
        serde_json::json!({"item": second}),
    )
    .await
    .expect("the server removes it");
}

/// The accounts a follow of `invocation` says the person has now: their addresses, or `None`
/// while it has said nothing yet.
fn followed_addresses(
    session: &DesktopSession,
    caller: &Caller,
    invocation: &InvocationId,
) -> Option<Vec<String>> {
    let state = session.observe(caller, invocation)?;
    let tilo_actions::InvocationUpdate::Item { value, .. } = state.update else {
        return None;
    };
    let accounts = value["accounts"].as_array()?.clone();
    Some(
        accounts
            .iter()
            .filter_map(|account| account["address"].as_str().map(str::to_owned))
            .collect(),
    )
}

/// Wait on the page's follow of `invocation` until it says `want`, as a Settings window waits:
/// each wait held by the server until the accounts move, never a poll.
async fn follow_until(
    session: &DesktopSession,
    caller: &Caller,
    invocation: &InvocationId,
    want: &[&str],
) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        // A page reads only what the server last said while it follows: one that stopped
        // following would read the same listing forever.
        let change = session
            .next_change(caller, invocation)
            .unwrap_or_else(|| panic!("the page stopped following the accounts before {want:?}"));
        let now = followed_addresses(session, caller, invocation);
        if now.as_deref().is_some_and(|now| now == want) {
            return;
        }
        assert!(Instant::now() < deadline, "never said {want:?}: {now:?}");
        tilo_test_env::within("the server says the accounts moved", change).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_following_the_accounts_hears_one_added_and_removed_elsewhere() {
    let hosted = Hosted::start().await;
    hosted
        .workspaces
        .sign_in_accounts_through(|_, _| Ok("signed in".into()));
    let session = page_desktop(&hosted);
    let person = hex::encode(hosted.root.verifying_key().to_bytes());
    let me = Caller::local_person(person).in_workspace(hosted.workspace.clone());
    let mail = |action| person_accounts::target(action);

    // Settings > Accounts follows them, as its pane does.
    let watch = InvocationId::new();
    let receipt = tilo_test_env::within(
        "the server admits the follow",
        session.invoke(
            &me,
            InvokeRequest {
                on_behalf_of: None,
                invocation: watch,
                target: mail(person_accounts::WATCH),
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            },
        ),
    )
    .await
    .expect("the session answers")
    .expect("the server follows the accounts");
    assert!(
        matches!(receipt.status, ReceiptStatus::Accepted),
        "a follow stays open: {receipt:?}"
    );
    follow_until(&session, &me, &watch, &[]).await;

    // The person takes a while to fill in the form (or to sign in in another tab): the server
    // holds the follow's wait for longer than the page waits for any one answer.
    let idle = session
        .next_change(&me, &watch)
        .expect("the page follows the accounts");
    let unanswered = tilo_session_actions::REPLY_DEADLINE.saturating_add(Duration::from_secs(5));
    tokio::time::timeout(unanswered, idle)
        .await
        .expect("an ask left unanswered past the deadline wakes its waiter");

    // Another call adds one: the follow hears it without the page asking again.
    let added = asks(
        &session,
        &me,
        mail(person_accounts::ADD_MAIL),
        serde_json::json!({
            "address": "ada@example.test",
            "displayName": "Ada",
            "incomingHost": "imap.provider.test",
            "outgoingHost": "smtp.provider.test",
            "password": PASSWORD,
        }),
    )
    .await
    .expect("the server keeps the account");
    follow_until(&session, &me, &watch, &["ada@example.test"]).await;

    // And hears it go.
    let id = added["id"].as_str().expect("its id").to_owned();
    asks(
        &session,
        &me,
        mail(person_accounts::REMOVE),
        serde_json::json!({"account": id}),
    )
    .await
    .expect("the server removes it");
    follow_until(&session, &me, &watch, &[]).await;
}
