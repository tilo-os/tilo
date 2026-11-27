//! A page's field filled from a vault item by a turn the server carries out: the first
//! fill of an item at a site waits for its person and releases nothing, their approve
//! releases that fill once, the grant their "always" gives is the item's own leave, which
//! they read and take back, and what is released is a password, a code or a value for
//! that one site and nothing else.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, and the server handle a test binds is kept until the test ends"
)]

use serde_json::{Value, json};
use tilo_action_policy::{ActionKind, Answer, KindSet};
use tilo_actions::InvocationId;
use tilo_actions::remote::ActionCallError as Refusal;
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest};
use tilo_user_identity::DeviceCert;
use tilo_workspace_records::vault::{self as workspace_vault, ItemRef};

use crate::fixtures::hosting_device;
use crate::fixtures::test_host as serving;
use crate::fixtures::{admit, caller, hosted};
use crate::hosted_use::turn_use_tests::{always, approved, asked_by, asks, fact, on_turn};
use crate::hosted_use::{HostedTurn, PageField, PageFill, PageText};
use crate::person_vault::vault_tests::{ask, files_holding};
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_service::workspaces::actions::ServiceWorkspaceActions as Service;

const PASSWORD: &str = "correct horse battery staple";
const SITE: &str = "https://example.test";
const PAGE_USE: &str = "tilo.vault/fill-page";
/// The seed RFC 6238 tests with, as the setup key a site shows for it.
const SEED: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

/// An item of `kind` kept for `place` by `who` from their own device, by its reference.
async fn kept(
    service: &Service,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    (kind, place, secret): (&str, &str, &str),
) -> ItemRef {
    let Ok(kept) = ask(
        service,
        workspace_id,
        who,
        workspace_vault::ADD,
        json!({"kind": kind, "belongsTo": [place], "secret": secret}),
    )
    .await
    else {
        unreachable!("the person keeps an item");
    };
    let Some(reference) = kept.get("reference").and_then(Value::as_str) else {
        unreachable!("an item kept answers its reference");
    };
    ItemRef::parse(reference).unwrap()
}

/// What `item` remembers, as its person reads it from their own device.
async fn remembered(
    service: &Service,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    item: &ItemRef,
) -> Value {
    let Ok(remembers) = ask(
        service,
        workspace_id,
        who,
        workspace_vault::USES,
        json!({"item": item}),
    )
    .await
    else {
        unreachable!("a person reads what their item remembers");
    };
    remembers
}

/// A page a turn asks to fill: its site, the item, and why.
struct FillPage {
    site: String,
    item: ItemRef,
    reason: String,
}

impl FillPage {
    fn page(&self) -> PageFill<'_> {
        PageFill {
            site: &self.site,
            item: &self.item,
            reason: &self.reason,
        }
    }
}

fn fill(site: &str, item: &ItemRef) -> FillPage {
    FillPage {
        site: site.to_owned(),
        item: item.clone(),
        reason: "To sign in for you.".to_owned(),
    }
}

/// The request that fills `wanted`, as a turn's tool makes it.
fn filling(wanted: &FillPage) -> InvokeRequest {
    let Value::Object(argument) =
        json!({"site": wanted.site, "item": wanted.item, "reason": wanted.reason})
    else {
        panic!("a fill reads as an object");
    };
    InvokeRequest {
        asks: false,
        invocation: InvocationId::new(),
        target: workspace_vault::target(workspace_vault::FILL_PAGE),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

async fn release(
    workspaces: &crate::PersonServices,
    turn: &HostedTurn<'_>,
    wanted: &FillPage,
) -> Result<PageText, Refusal> {
    released(workspaces, turn, wanted, &filling(wanted)).await
}

/// What `request`, a fill of `wanted`, releases.
async fn released(
    workspaces: &crate::PersonServices,
    turn: &HostedTurn<'_>,
    wanted: &FillPage,
    request: &InvokeRequest,
) -> Result<PageText, Refusal> {
    workspaces
        .release_for_turn(turn, wanted.page(), request)
        .await
}

/// The text a fill was released, when it was.
fn text(released: Result<PageText, Refusal>) -> PageText {
    released.unwrap_or_else(|error| panic!("the text is released: {error:?}"))
}

/// The grant "always" gives the agent `agent` to fill `item` at `SITE`.
async fn always_fill(
    workspaces: &crate::PersonServices,
    (workspace, owner, ada): (&str, &DeviceCert, &DeviceCert),
    agent: &str,
    item: &ItemRef,
) {
    always(
        workspaces,
        (workspace, owner, ada),
        agent,
        (
            &workspace_vault::target(workspace_vault::FILL_PAGE),
            &json!(item),
            &[SITE],
        ),
    )
    .await;
}

/// Why a fill was refused, when it was.
fn refusal(released: Result<PageText, Refusal>) -> String {
    match released {
        Err(Refusal::Refused { reason }) => reason,
        other => panic!("the fill is refused: {other:?}"),
    }
}

/// A workspace policy whose grants on the agent `caller` acts as are `rules`, set by its own
/// person.
fn narrowing(
    workspace: &str,
    caller: &Caller,
    rules: &[tilo_action_policy::Rule],
) -> tilo_action_policy::WorkspacePolicy {
    let mut policy = tilo_action_policy::WorkspacePolicy::new(workspace, "owner");
    let (agent, person) = crate::hosted_use::agent_of(caller).unwrap_or_default();
    for (index, rule) in rules.iter().enumerate() {
        policy.grants.push(tilo_action_policy::Grant::new(
            format!("test-narrowing:{index}"),
            tilo_action_policy::Grantee::agent(agent, person),
            rule.node.clone(),
            rule.kinds,
            rule.answer,
            tilo_action_policy::Grantee::person(person),
        ));
    }
    policy
}

/// The first fill of an item at a site waits for its person and releases nothing. What they
/// are shown names the site and withholds the item; the same fill asked again waits again; a
/// decline never admits it. Approved, that fill is released once, written down, and the one
/// after asks again; another agent of hers is still asked.
#[tokio::test]
async fn the_first_fill_at_a_site_asks_and_an_allow_releases_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = serving(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let login = kept(&service, workspace_id, hers, ("login", SITE, PASSWORD)).await;
    let scout = on_turn(&ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
    };
    assert_eq!(
        workspaces.uses_offered(&turn).await.len(),
        2,
        "no listing offers the page's use"
    );

    let wanted = fill(SITE, &login);
    let first = filling(&wanted);
    let facts = asks(released(workspaces, &turn, &wanted, &first).await);
    assert!(asked_by(&facts, "scout", "Scout"), "{facts:?}");
    assert_eq!(
        tilo_actions::remote::asked::asked_person(&scout),
        Some(hex::encode(ada.user_public_key))
    );
    assert_eq!(facts.words, "To sign in for you.");
    assert_eq!(fact(&facts, "Item"), "login");
    assert_eq!(fact(&facts, "Goes to"), SITE);
    assert_eq!(fact(&facts, "Site"), SITE);
    assert_eq!(facts.verb, PAGE_USE);
    let shown = serde_json::to_string(&facts).unwrap();
    assert!(
        !shown.contains(login.id()) && !shown.contains(PASSWORD),
        "{shown}"
    );
    assert_eq!(
        remembered(&service, workspace_id, hers, &login).await,
        json!({"uses": []})
    );

    // Asked the same again, it waits again; declined, nothing is released.
    assert_eq!(asks(release(workspaces, &turn, &wanted).await), facts);
    assert_eq!(
        remembered(&service, workspace_id, hers, &login).await,
        json!({"uses": []})
    );
    // Another agent of hers is still asked: the approve was for one call.
    let other = on_turn(&ada, &workspace, "other");
    let others = HostedTurn {
        caller: &other,
        ..turn
    };
    asks(release(workspaces, &others, &wanted).await);

    // That agent's next fill there is released, for a password field, and written down.
    let filled = text(released(workspaces, &turn, &wanted, &approved(&first, &workspace)).await);
    assert_eq!(filled.text(), PASSWORD);
    assert_eq!(filled.field(), PageField::Password);
    assert_eq!(filled.field().as_str(), "password");
    let printed = format!("{filled:?}");
    assert!(!printed.contains("horse"), "{printed}");
    let remembers = remembered(&service, workspace_id, hers, &login).await;
    assert_eq!(
        remembers["uses"].as_array().unwrap().len(),
        1,
        "{remembers}"
    );
    assert_eq!(remembers["uses"][0]["places"], json!([SITE]));
    assert_eq!(remembers["uses"][0]["by"], "Scout");
    assert_eq!(remembers.get("always"), None, "once leaves no leave");

    // Once is once: the fill after it asks again.
    asks(release(workspaces, &turn, &wanted).await);
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}

/// With the grant "always" gives, that agent fills that item at that site without a question,
/// each fill written down, until its person takes the leave back.
#[tokio::test]
async fn after_always_the_agent_fills_there_alone_until_the_leave_is_taken_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = serving(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let login = kept(&service, workspace_id, hers, ("login", SITE, PASSWORD)).await;
    let scout = on_turn(&ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
    };
    let wanted = fill(SITE, &login);

    asks(release(workspaces, &turn, &wanted).await);
    always_fill(workspaces, (&workspace, &owner, &ada), "scout", &login).await;
    let remembers = remembered(&service, workspace_id, hers, &login).await;
    assert_eq!(
        remembers["always"],
        json!([{
            "agent": "scout",
            "using": PAGE_USE,
            "places": [SITE],
            "since": remembers["always"][0]["since"],
        }])
    );
    assert!(
        remembers["always"][0]["since"]
            .as_u64()
            .is_some_and(|since| since > 0)
    );
    assert_eq!(remembers["uses"], json!([]), "the leave used nothing");

    for _ in 0..2 {
        assert_eq!(
            text(release(workspaces, &turn, &wanted).await).text(),
            PASSWORD
        );
    }
    let remembers = remembered(&service, workspace_id, hers, &login).await;
    assert_eq!(
        remembers["uses"].as_array().unwrap().len(),
        2,
        "{remembers}"
    );

    // The leave is for that site: the same item at another site it belongs to nowhere.
    let elsewhere = refusal(
        release(
            workspaces,
            &turn,
            &fill("https://login.example.test", &login),
        )
        .await,
    );
    assert!(!elsewhere.contains(PASSWORD), "{elsewhere}");

    // She takes the leave back from her own device, and then her agent is asked again.
    let taken = ask(
        &service,
        workspace_id,
        hers,
        workspace_vault::TAKE_BACK,
        json!({"item": login}),
    )
    .await
    .expect("she takes her leave back");
    assert_eq!(taken.get("always"), None, "{taken}");
    asks(release(workspaces, &turn, &wanted).await);
    server.shutdown().await;
}

/// A code generator fills the code of this moment and never its seed, a value fills a
/// value, and what is no sign-in, a site an item does not name, a page that did not
/// arrive over https and a reference that names nothing are refused before anyone is
/// asked.
#[tokio::test]
async fn a_code_and_a_value_are_released_and_what_the_vault_refuses_asks_nobody() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = serving(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let generator = kept(&service, workspace_id, hers, ("code-generator", SITE, SEED)).await;
    let number = kept(
        &service,
        workspace_id,
        hers,
        ("value", SITE, "4111 1111 1111 1111"),
    )
    .await;
    let api_key = kept(
        &service,
        workspace_id,
        hers,
        ("key", SITE, "sk-not-for-a-page"),
    )
    .await;
    let scout = on_turn(&ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
    };

    for (item, kind) in [(&generator, "code-generator"), (&number, "value")] {
        let wanted = fill(SITE, item);
        let facts = asks(release(workspaces, &turn, &wanted).await);
        assert_eq!(fact(&facts, "Item"), kind);
        always_fill(workspaces, (&workspace, &owner, &ada), "scout", item).await;
    }
    let code = text(release(workspaces, &turn, &fill(SITE, &generator)).await);
    assert_eq!(code.field(), PageField::Code);
    assert_eq!(code.text().len(), 6);
    assert!(code.text().bytes().all(|digit| digit.is_ascii_digit()));
    let printed = format!("{code:?}");
    assert!(!printed.contains(code.text()), "{printed}");
    let value = text(release(workspaces, &turn, &fill(SITE, &number)).await);
    assert_eq!(value.field(), PageField::Value);
    assert_eq!(value.text(), "4111 1111 1111 1111");
    assert!(files_holding(dir.path(), SEED.as_bytes()).is_empty());

    // Refused before anyone is asked, in words that name no secret.
    let nobody = ItemRef::from_id(&"0".repeat(32)).unwrap();
    for (why, wanted) in [
        ("a key is no sign-in", fill(SITE, &api_key)),
        (
            "a site the item does not name",
            fill("https://example.test.elsewhere.test", &generator),
        ),
        ("another port", fill("https://example.test:8443", &number)),
        ("not over https", fill("http://example.test", &number)),
        ("no address at all", fill("example", &number)),
        ("a reference that names nothing", fill(SITE, &nobody)),
    ] {
        let reason = refusal(release(workspaces, &turn, &wanted).await);
        assert!(
            !reason.contains(SEED) && !reason.contains("4111") && !reason.contains("sk-not"),
            "{why}: {reason}"
        );
    }
    server.shutdown().await;
}

/// Only a turn this server carries out fills from the workspace's vault, whichever member
/// asked it; an agent that anything narrows fills only where a rule of its bound names exactly
/// this use, and is then asked first like any agent.
#[tokio::test]
async fn any_members_turn_fills_and_a_narrowed_agent_needs_the_rule() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = serving(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bob = admit(workspaces, &workspace, &owner, 2).await;
    let as_ada = caller(&ada);
    let login = kept(
        &service,
        workspace_id,
        (&ada, &as_ada),
        ("login", SITE, PASSWORD),
    )
    .await;
    let wanted = fill(SITE, &login);
    let scout = on_turn(&ada, &workspace, "scout");

    // Herself, asking as herself, is no turn this server carries out.
    let herself = as_ada.clone().in_workspace(workspace.as_str());
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &herself,
        policy: None,
    };
    assert!(matches!(
        release(workspaces, &turn, &wanted).await,
        Err(Refusal::UnknownAction)
    ));

    let outside = KindSet::of(ActionKind::Outside);
    let named = |action: &str| {
        tilo_automation::action_rule(
            workspace_vault::PROVIDER,
            workspace_vault::RESOURCE,
            None,
            action,
            outside,
            Answer::Allow,
        )
    };
    for (why, bound) in [
        ("no rule at all", Vec::new()),
        (
            "a rule for another action",
            vec![named(workspace_vault::LIST)],
        ),
    ] {
        let policy = narrowing(&workspace_id.to_string(), &scout, &bound);
        let turn = HostedTurn {
            workspace: workspace_id,
            caller: &scout,
            policy: Some(&policy),
        };
        assert!(
            matches!(
                release(workspaces, &turn, &wanted).await,
                Err(Refusal::NotPermitted)
            ),
            "{why}"
        );
    }
    let bound = vec![named(workspace_vault::FILL_PAGE)];
    let policy = narrowing(&workspace_id.to_string(), &scout, &bound);
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: Some(&policy),
    };
    asks(release(workspaces, &turn, &wanted).await);

    // Bob's ask of the same agent fills from the same workspace item, and is put to him.
    let bobs = on_turn(&bob, &workspace, "scout");
    let his = HostedTurn {
        workspace: workspace_id,
        caller: &bobs,
        policy: None,
    };
    asks(release(workspaces, &his, &wanted).await);
    assert_eq!(
        tilo_actions::remote::asked::asked_person(&bobs),
        Some(hex::encode(bob.user_public_key))
    );
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}
