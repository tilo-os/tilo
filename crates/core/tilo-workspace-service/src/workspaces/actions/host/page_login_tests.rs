//! The logins a browser window's page signs in with, kept in the workspace's vault
//! ([`crate::workspaces::page_logins`]): the window keeps the login its page sent, is told the
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

use crate::workspace_vault::{self, ItemRef, PageFilled, PageLogin, PageLogins};
use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::{ActionCallError, InvokeArgument, InvokeRequest, RemoteActionTarget};

use super::super::tests::{caller, hosted};
use super::release_tests::{approve_and_grant, asked_records, refusal};
use super::vault_tests::ask;
use crate::workspaces::page_logins;
use crate::workspaces::tests::{hosting_device, test_host};
use crate::workspaces::vault_release::AppProcess;

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
    let workspaces = server.workspaces();
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
    let page = |request: InvokeRequest| {
        let (workspace, window) = (&workspace, &window);
        async move {
            workspaces
                .page_login((BROWSER, workspace), window, &request)
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
    let person = caller(&owner).in_workspace(&workspace);
    super::super::tests::settled(
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

    // A tilolang process and a person's own call are answered nothing.
    let shared = refusal(
        workspaces
            .page_login(
                (BROWSER, &workspace),
                &AppProcess::Tilolang,
                &fill(&kept.reference, SHOP),
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
