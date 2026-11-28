//! The logins a browser window's page signs in with, kept in the workspace's vault
//! ([`crate::page_logins`]): the window keeps the login its page sent, is told the
//! logins its site has by name alone, fills its own unasked, and fills the person's own login
//! only once they allowed it. A secret travels between the window and the vault alone.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    clippy::large_futures,
    reason = "in a test these are the assertion, and each test keeps its host to the end"
)]

use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::{ActionCallError, InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_workspace_records::vault::{
    self as workspace_vault, ItemRef, PageFilled, PageLogin, PageLogins,
};

use crate::fixtures::{caller, hosted};
use crate::fixtures::{hosting_device, test_host};
use crate::page_logins;
use crate::person_vault::vault_tests::ask;
use crate::vault_release::AppProcess;
use crate::vault_release::release_tests::{approve_and_grant, asked_records, refusal};
use tilo_workspace_service::workspaces::fixtures::given;

/// The app whose window shows the page.
const BROWSER: &str = "so.tilo.browser";
/// The site the page is at.
const SHOP: &str = "https://shop.example.test";
/// What the person typed into the shop's form: made up.
const TYPED: &str = "made-up-typed-password-41";
/// What the person keeps for the shop in Passwords themselves: made up.
const THEIRS: &str = "made-up-kept-password-17";

/// `action` at the vault, as the window's process calls it with `argument`.
fn call(target: RemoteActionTarget, argument: &Value) -> InvokeRequest {
    InvokeRequest {
        asks: false,
        invocation: InvocationId::new(),
        target,
        argument: InvokeArgument::Json(argument.as_object().cloned().unwrap()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

fn at_vault(action: &str, argument: &Value) -> InvokeRequest {
    call(workspace_vault::target(action), argument)
}

fn fill(item: &ItemRef, site: &str) -> InvokeRequest {
    call(
        workspace_vault::ItemRef::target(item, workspace_vault::PAGE_FILL),
        &json!({"site": site}),
    )
}

/// The browser's window keeps the login its page sent, reads its site's logins by name, fills its
/// own unasked, and fills the person's own only after they allowed that one login. Nothing goes
/// anywhere else, and no secret is in a listing, a question or the call that kept it.
#[tokio::test]
async fn a_windows_page_signs_in_with_the_vaults_logins_through_its_grants() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let added = ask(
        &service,
        workspace_id,
        (&owner, &caller(&owner)),
        workspace_vault::ADD,
        json!({"kind": "login", "belongsTo": [SHOP], "username": "ada", "secret": THEIRS}),
    )
    .await
    .expect("the person keeps a login");
    let theirs = ItemRef::parse(added["reference"].as_str().unwrap()).unwrap();
    let window = AppProcess::Unplaced;
    // Each step is asked by the person's own call of the Browser's action.
    let person = caller(&owner).in_workspace(&workspace);
    let page = |request: InvokeRequest| {
        let (workspace, window, person) = (&workspace, &window, &person);
        async move {
            workspaces
                .page_login((BROWSER, workspace), window, &request, Some(person))
                .await
        }
    };

    // The window keeps what the page's form sent, as the Browser's own sign-in.
    let keeping = json!({
        "site": format!("{SHOP}/account/login"),
        "username": "ada.work",
        "secret": TYPED,
    });
    let Value::Object(arguments) = &keeping else {
        unreachable!("a keep's arguments are an object")
    };
    let decoded = workspace_vault::decoded(workspace_vault::KEEP_PAGE_LOGIN, arguments).unwrap();
    assert!(!format!("{decoded:?}").contains(TYPED));
    let kept: PageLogin = serde_json::from_value(
        page(at_vault(workspace_vault::KEEP_PAGE_LOGIN, &keeping))
            .await
            .expect("the window keeps its page's login"),
    )
    .unwrap();
    assert!(kept.own);
    assert_eq!(kept.username, "ada.work");
    assert_eq!(kept.belongs_to, [SHOP]);
    let items = workspaces.open_vault(&workspace).await.unwrap().items();
    let id = accounts::vault_items::ItemId::parse(kept.reference.id()).unwrap();
    let facts = items.facts().into_iter().find(|facts| facts.id == id);
    assert_eq!(
        facts.unwrap().tags,
        [
            workspace_vault::app_tag(BROWSER),
            workspace_vault::purpose_tag(workspace_vault::SIGN_IN)
        ],
        "the item is tagged with its app and purpose"
    );

    // The site's logins, by name alone.
    let listed = page(at_vault(
        workspace_vault::PAGE_LOGINS,
        &json!({"site": SHOP}),
    ))
    .await
    .unwrap();
    assert!(!listed.to_string().contains(TYPED) && !listed.to_string().contains(THEIRS));
    let listed: PageLogins = serde_json::from_value(listed).unwrap();
    let names: Vec<(&str, bool)> = listed
        .logins
        .iter()
        .map(|login| (login.username.as_str(), login.own))
        .collect();
    assert_eq!(names, [("ada", false), ("ada.work", true)]);
    let elsewhere: PageLogins = serde_json::from_value(
        page(at_vault(
            workspace_vault::PAGE_LOGINS,
            &json!({"site": "https://other.example.test"}),
        ))
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(elsewhere.logins.is_empty());

    // Its own login fills unasked, at its site alone.
    let filled: PageFilled =
        serde_json::from_value(page(fill(&kept.reference, SHOP)).await.unwrap()).unwrap();
    assert_eq!(
        (filled.username.as_str(), filled.secret.as_str()),
        ("ada.work", TYPED)
    );
    assert!(!format!("{filled:?}").contains(TYPED));
    let wrong = refusal(page(fill(&kept.reference, "https://evil.example.test")).await);
    assert_eq!(wrong, "That item belongs somewhere else");

    // The person's own login is asked for once, fills nothing meanwhile, and fills once they
    // allowed that one login.
    let asked = refusal(page(fill(&theirs, SHOP)).await);
    assert_eq!(asked, page_logins::asked_first(BROWSER));
    let again = refusal(page(fill(&theirs, SHOP)).await);
    assert_eq!(again, asked);
    let store = workspaces.asked_store().unwrap();
    let waiting = store.waiting_in(&workspace);
    assert_eq!(waiting.len(), 1, "one question for the one login");
    let shown = asked_records(workspaces, &workspace);
    assert!(shown.contains("ada") && !shown.contains(THEIRS) && !shown.contains(TYPED));
    // Not its own, not allowed: it neither changes nor forgets it.
    let forget_theirs = refusal(
        page(at_vault(
            workspace_vault::FORGET_PAGE_LOGIN,
            &json!({"site": SHOP, "item": theirs}),
        ))
        .await,
    );
    assert_eq!(forget_theirs, page_logins::not_its_own(BROWSER));
    crate::fixtures::settled(
        workspaces.resolve_asked(&person, &approve_and_grant(&waiting[0].asked.record)),
    )
    .await
    .expect("their answer is taken");
    let filled: PageFilled =
        serde_json::from_value(page(fill(&theirs, SHOP)).await.unwrap()).unwrap();
    assert_eq!(
        (filled.username.as_str(), filled.secret.as_str()),
        ("ada", THEIRS)
    );
    let uses = workspaces
        .open_vault(&workspace)
        .await
        .unwrap()
        .items()
        .uses(&accounts::vault_items::ItemId::parse(theirs.id()).unwrap())
        .unwrap();
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].by, BROWSER);
    assert_eq!(uses[0].using, "tilo.vault/page-fill");

    // A fill is decided as whoever made the call that asked for it, too. One no running call
    // asked for fills nothing, and an agent fills only a login its person granted it, even the
    // Browser's own.
    let asked_by = |by: Option<&tilo_actions::remote::Caller>, item: &ItemRef| {
        let request = fill(item, SHOP);
        let (workspace, window) = (&workspace, &window);
        let by = by.cloned();
        async move {
            workspaces
                .page_login((BROWSER, workspace), window, &request, by.as_ref())
                .await
        }
    };
    let nobody = refusal(asked_by(None, &kept.reference).await);
    assert_eq!(nobody, page_logins::ASKED_BY_NOBODY);
    let scout = person.clone().delegated_to("scout");
    let ungranted = refusal(asked_by(Some(&scout), &kept.reference).await);
    assert_eq!(ungranted, page_logins::not_granted("scout"));
    assert!(!ungranted.contains(TYPED));
    let person_id = hex::encode(owner.user_public_key);
    given(
        workspaces,
        &workspace,
        &owner,
        vec![tilo_action_policy::Grant::new(
            "test:scout-fills-the-browsers-login",
            tilo_action_policy::Grantee::agent("scout", person_id.clone()),
            super::fill_node(&workspace, &kept.reference),
            tilo_action_types::KindSet::ALL,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(person_id),
        )],
    )
    .await;
    let filled: PageFilled = serde_json::from_value(
        asked_by(Some(&scout), &kept.reference)
            .await
            .expect("the granted agent's call fills the login"),
    )
    .unwrap();
    assert_eq!(filled.username, "ada.work");
    let other_login = refusal(asked_by(Some(&scout), &theirs).await);
    assert_eq!(
        other_login,
        page_logins::not_granted("scout"),
        "a grant names one login"
    );

    // A tilolang process and a person's own call are answered nothing.
    let shared = refusal(
        workspaces
            .page_login(
                (BROWSER, &workspace),
                &AppProcess::Tilolang,
                &fill(&kept.reference, SHOP),
                Some(&person),
            )
            .await,
    );
    assert_eq!(shared, page_logins::from_a_tilolang_process(BROWSER));
    let by_person = ask(
        &service,
        workspace_id,
        (&owner, &caller(&owner)),
        workspace_vault::PAGE_LOGINS,
        json!({"site": SHOP}),
    )
    .await;
    assert!(
        matches!(&by_person, Err(ActionCallError::Refused { reason }) if reason == page_logins::NOT_A_WINDOW),
        "{by_person:?}"
    );

    // Forgetting its own login leaves the person's.
    let left: PageLogins = serde_json::from_value(
        page(at_vault(
            workspace_vault::FORGET_PAGE_LOGIN,
            &json!({"site": SHOP, "item": kept.reference}),
        ))
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        left.logins
            .iter()
            .map(|login| &login.reference)
            .collect::<Vec<_>>(),
        [&theirs]
    );
}

/// The sites the person brought their sign-ins of open signed in in their own Browser's window,
/// which is handed each sign-in its profile does not hold, and in no other app's window or a
/// tilolang process.
#[tokio::test]
async fn the_browsers_window_alone_is_handed_the_persons_website_sign_ins() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, _, _) = hosted(workspaces).await;
    let cookies = json!([{
        "host": "shop.example.test", "domain": "shop.example.test", "host_only": true,
        "name": "session", "value": "made-up-cookie-23", "path": "/",
        "expires_unix_seconds": null, "secure": true, "http_only": true, "same_site": "lax",
    }]);
    workspaces
        .keep_website_sign_in(
            &workspace,
            owner.user_public_key,
            ("web-firefox-1", "Sign-ins from Firefox", "web", SHOP),
            &zeroize::Zeroizing::new(cookies.to_string()),
        )
        .await
        .expect("the person's sign-ins are kept");
    let asking = |app: &'static str, process: AppProcess, held: Value| {
        let workspace = &workspace;
        async move {
            let request = at_vault(workspace_vault::WEBSITE_SIGN_INS, &json!({"held": held}));
            workspaces
                .page_login((app, workspace), &process, &request, None)
                .await
        }
    };
    let handed: workspace_vault::WebsiteSignIns = serde_json::from_value(
        asking(BROWSER, AppProcess::Unplaced, json!([]))
            .await
            .expect("Browser's window is handed them"),
    )
    .unwrap();
    assert_eq!(handed.sign_ins.len(), 1);
    assert!(handed.sign_ins[0].session.contains("made-up-cookie-23"));
    let held = handed.sign_ins[0].held.clone();
    let again: workspace_vault::WebsiteSignIns = serde_json::from_value(
        asking(BROWSER, AppProcess::Unplaced, json!([held]))
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        again.sign_ins.is_empty(),
        "what the profile holds is not handed again"
    );
    assert_eq!(
        refusal(asking("so.tilo.notes", AppProcess::Unplaced, json!([])).await),
        page_logins::NOT_BROWSER
    );
    assert_eq!(
        refusal(asking(BROWSER, AppProcess::Tilolang, json!([])).await),
        page_logins::from_a_tilolang_process(BROWSER)
    );
}

/// The website accounts the person brought (Slack) are listed to their own Browser's window,
/// which opens each on a profile of its own with the account's sign-in; their sign-ins of every
/// site are no account of their own, and no other app's window or tilolang process is told of
/// any.
#[tokio::test]
async fn the_browsers_window_alone_lists_and_opens_the_persons_website_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, _, _) = hosted(workspaces).await;
    let cookie = |host: &str, value: &str| {
        json!([{
            "host": host, "domain": host, "host_only": true,
            "name": "d", "value": value, "path": "/",
            "expires_unix_seconds": null, "secure": true, "http_only": true, "same_site": "lax",
        }])
        .to_string()
    };
    for (id, title, service, site, cookies) in [
        (
            "slack-firefox-1",
            "Slack (Firefox)",
            "slack",
            "https://app.slack.com",
            cookie("app.slack.com", "made-up-slack-cookie"),
        ),
        (
            "web-firefox-1",
            "Sign-ins from Firefox",
            "web",
            SHOP,
            cookie("shop.example.test", "made-up-shop-cookie"),
        ),
    ] {
        workspaces
            .keep_website_sign_in(
                &workspace,
                owner.user_public_key,
                (id, title, service, site),
                &zeroize::Zeroizing::new(cookies),
            )
            .await
            .expect("the person's sign-ins are kept");
    }
    let asking = |app: &'static str, process: AppProcess, action: &'static str, argument: Value| {
        let workspace = &workspace;
        async move {
            let request = at_vault(action, &argument);
            workspaces
                .page_login((app, workspace), &process, &request, None)
                .await
        }
    };
    let listed: workspace_vault::WebsiteAccounts = serde_json::from_value(
        asking(
            BROWSER,
            AppProcess::Unplaced,
            workspace_vault::WEBSITE_ACCOUNTS,
            json!({}),
        )
        .await
        .expect("Browser's window is told of them"),
    )
    .unwrap();
    assert_eq!(
        listed.accounts,
        [workspace_vault::WebsiteAccount {
            id: "slack-firefox-1".to_owned(),
            service: "slack".to_owned(),
            title: "Slack (Firefox)".to_owned(),
        }]
    );
    let opened: workspace_vault::WebsiteAccountSession = serde_json::from_value(
        asking(
            BROWSER,
            AppProcess::Unplaced,
            workspace_vault::WEBSITE_ACCOUNT,
            json!({"account": "slack-firefox-1"}),
        )
        .await
        .expect("Browser's window opens it"),
    )
    .unwrap();
    assert!(opened.session.contains("made-up-slack-cookie"));
    assert!(!format!("{opened:?}").contains("made-up-slack-cookie"));
    assert_eq!(
        refusal(
            asking(
                BROWSER,
                AppProcess::Unplaced,
                workspace_vault::WEBSITE_ACCOUNT,
                json!({"account": "web-firefox-1"}),
            )
            .await
        ),
        crate::website_session_asks::UNAVAILABLE
    );
    for (action, argument) in [
        (workspace_vault::WEBSITE_ACCOUNTS, json!({})),
        (
            workspace_vault::WEBSITE_ACCOUNT,
            json!({"account": "slack-firefox-1"}),
        ),
    ] {
        assert_eq!(
            refusal(
                asking(
                    "so.tilo.notes",
                    AppProcess::Unplaced,
                    action,
                    argument.clone()
                )
                .await
            ),
            page_logins::NOT_BROWSER
        );
        assert_eq!(
            refusal(asking(BROWSER, AppProcess::Tilolang, action, argument).await),
            page_logins::from_a_tilolang_process(BROWSER)
        );
    }
}
