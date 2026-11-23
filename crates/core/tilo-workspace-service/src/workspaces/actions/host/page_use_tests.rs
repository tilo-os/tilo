//! A page's field filled from a vault item by a turn the server carries out: the first
//! fill of an item at a site asks its person and releases nothing, their allow lets that
//! agent's next fill there through once, their "Always" is the item's own leave, which
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
use tilo_actions::remote::Caller;
use tilo_actions::remote::person_vault::{self, FillPage, ItemRef};
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions as Service;
use super::super::tests::{admit, caller, governed};
use super::turn_use_tests::on_turn;
use super::vault_tests::{ask, files_holding};
use crate::workspaces::fixtures::test_host as serving;
use crate::workspaces::hosted_use::{
    HostedTurn, OpenAsk, PageField, PageText, TurnRelease, UseAnswer,
};
use crate::workspaces::tests::governor;
use crate::workspaces::{WorkspaceId, Workspaces};

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
    let kept = ask(
        service,
        workspace_id,
        who,
        person_vault::ADD,
        json!({"kind": kind, "belongsTo": [place], "secret": secret}),
    )
    .await
    .expect("the person keeps an item");
    ItemRef::parse(kept["reference"].as_str().unwrap()).unwrap()
}

/// What `item` remembers, as its person reads it from their own device.
async fn remembered(
    service: &Service,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    item: &ItemRef,
) -> Value {
    ask(
        service,
        workspace_id,
        who,
        person_vault::USES,
        json!({"item": item}),
    )
    .await
    .expect("a person reads what their item remembers")
}

fn fill(site: &str, item: &ItemRef) -> FillPage {
    FillPage {
        site: site.to_owned(),
        item: item.clone(),
        reason: "To sign in for you.".to_owned(),
    }
}

async fn release(
    workspaces: &Workspaces,
    turn: &HostedTurn<'_>,
    wanted: &FillPage,
) -> Result<TurnRelease, Refusal> {
    workspaces
        .release_for_turn(turn, wanted, InvocationId::new(), json!({"part": "p0"}))
        .await
}

/// The ask a fill answered with, when it asked.
fn asked(released: Result<TurnRelease, Refusal>) -> OpenAsk {
    match released {
        Ok(TurnRelease::Asks(open)) => *open,
        other => panic!("the person is asked first: {other:?}"),
    }
}

/// The text a fill was released, when it was.
fn text(released: Result<TurnRelease, Refusal>) -> PageText {
    match released {
        Ok(TurnRelease::Released(text)) => text,
        other => panic!("the text is released: {other:?}"),
    }
}

/// Why a fill was refused, when it was.
fn refusal(released: Result<TurnRelease, Refusal>) -> String {
    match released {
        Err(Refusal::Refused { reason }) => reason,
        other => panic!("the fill is refused: {other:?}"),
    }
}

/// The first fill of an item at a site asks its person and releases nothing. What is kept
/// for the question names the site and withholds the item; the same fill asked again does
/// not ask twice; a "no" releases nothing. Allowed once, that agent's next fill there is
/// released once, written down, and the one after asks again.
#[tokio::test]
async fn the_first_fill_at_a_site_asks_and_an_allow_releases_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = serving(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let login = kept(&service, workspace_id, hers, ("login", SITE, PASSWORD)).await;
    let scout = on_turn(&ada, &workspace, "scout");
    let asker = hex::encode(ada.user_public_key);
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        bound: None,
    };
    assert_eq!(
        workspaces.uses_offered(&turn).await.len(),
        2,
        "no listing offers the page's use"
    );

    let wanted = fill(SITE, &login);
    let open = asked(release(workspaces, &turn, &wanted).await);
    assert_eq!(open.agent, "scout");
    assert_eq!(open.person, asker);
    assert_eq!(open.reason, "To sign in for you.");
    assert_eq!(open.asked_in, json!({"part": "p0"}));
    assert_eq!(
        open.detail["use"],
        json!({
            "ask": open.ask,
            "item": {"kind": "login", "belongsTo": [SITE]},
            "site": {"name": "example.test", "origin": SITE},
        })
    );
    let shown = open.detail.to_string();
    assert!(
        !shown.contains(login.id()) && !shown.contains(PASSWORD),
        "{shown}"
    );
    assert_eq!(
        remembered(&service, workspace_id, hers, &login).await,
        json!({"uses": []})
    );

    // Asked the same again, the one question stands; a "no" releases nothing.
    assert!(matches!(
        release(workspaces, &turn, &wanted).await,
        Ok(TurnRelease::Waits)
    ));
    assert_eq!(
        workspaces
            .answer_use(&turn, &open.ask, &open.detail, UseAnswer::No)
            .await,
        Ok(None)
    );
    let open = asked(release(workspaces, &turn, &wanted).await);

    // Allowed once: nothing is filled by the answer, and no leave is kept with the item.
    assert_eq!(
        workspaces
            .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Once)
            .await,
        Ok(Some(json!({"allowed": "once", "site": SITE})))
    );
    assert_eq!(
        remembered(&service, workspace_id, hers, &login).await,
        json!({"uses": []})
    );
    // Another agent of hers is still asked: the allow names one agent.
    let other = on_turn(&ada, &workspace, "other");
    let others = HostedTurn {
        caller: &other,
        ..turn
    };
    let theirs = asked(release(workspaces, &others, &wanted).await);
    workspaces.use_not_asked(&theirs);

    // That agent's next fill there is released, for a password field, and written down.
    let released = text(release(workspaces, &turn, &wanted).await);
    assert_eq!(released.text(), PASSWORD);
    assert_eq!(released.field(), PageField::Password);
    assert_eq!(released.field().as_str(), "password");
    let printed = format!("{released:?}");
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
    asked(release(workspaces, &turn, &wanted).await);
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}

/// After "Always" that agent fills that item at that site without a question, each fill
/// written down, until its person takes the leave back.
#[tokio::test]
async fn after_always_the_agent_fills_there_alone_until_the_leave_is_taken_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = serving(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let login = kept(&service, workspace_id, hers, ("login", SITE, PASSWORD)).await;
    let scout = on_turn(&ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        bound: None,
    };
    let wanted = fill(SITE, &login);

    let open = asked(release(workspaces, &turn, &wanted).await);
    assert_eq!(
        workspaces
            .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Always)
            .await,
        Ok(Some(json!({"allowed": "always", "site": SITE})))
    );
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
    assert!(workspaces.open_use_asks(&workspace).is_empty());
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
        person_vault::TAKE_BACK,
        json!({"item": login}),
    )
    .await
    .expect("she takes her leave back");
    assert_eq!(taken.get("always"), None, "{taken}");
    asked(release(workspaces, &turn, &wanted).await);
    server.shutdown().await;
}

/// A code generator fills the code of this moment and never its seed, a value fills a
/// value, and what is no sign-in, a site an item does not name, a page that did not
/// arrive over https and a reference that names nothing are refused before anyone is
/// asked.
#[tokio::test]
async fn a_code_and_a_value_are_released_and_what_the_vault_refuses_asks_nobody() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = serving(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
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
        bound: None,
    };

    for (item, kind) in [(&generator, "code-generator"), (&number, "value")] {
        let wanted = fill(SITE, item);
        let open = asked(release(workspaces, &turn, &wanted).await);
        assert_eq!(open.detail["use"]["item"]["kind"], kind);
        workspaces
            .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Always)
            .await
            .expect("allowed always")
            .expect("it says what was allowed");
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
    assert!(
        workspaces.open_use_asks(&workspace).is_empty(),
        "nobody was asked"
    );
    server.shutdown().await;
}

/// Only a turn this server carries out fills from the workspace's vault, whichever member
/// asked it; an agent that anything narrows fills only where a rule of its bound names exactly
/// this use, and is then asked first like any agent.
#[tokio::test]
async fn any_members_turn_fills_and_a_narrowed_agent_needs_the_rule() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = serving(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
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
        bound: None,
    };
    assert!(matches!(
        release(workspaces, &turn, &wanted).await,
        Err(Refusal::UnknownAction)
    ));

    let outside = KindSet::of(ActionKind::Outside);
    let named = |action: &str| {
        tilo_automation::action_rule(
            person_vault::PROVIDER,
            person_vault::RESOURCE,
            None,
            action,
            outside,
            Answer::Allow,
        )
    };
    for (why, bound) in [
        ("no rule at all", Vec::new()),
        ("a rule for another action", vec![named(person_vault::LIST)]),
    ] {
        let turn = HostedTurn {
            workspace: workspace_id,
            caller: &scout,
            bound: Some(&bound),
        };
        assert!(
            matches!(
                release(workspaces, &turn, &wanted).await,
                Err(Refusal::NotPermitted)
            ),
            "{why}"
        );
    }
    assert!(
        workspaces.open_use_asks(&workspace).is_empty(),
        "nobody was asked"
    );
    let bound = vec![named(person_vault::FILL_PAGE)];
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        bound: Some(&bound),
    };
    asked(release(workspaces, &turn, &wanted).await);

    // Bob's ask of the same agent fills from the same workspace item, and is put to him.
    let bobs = on_turn(&bob, &workspace, "scout");
    let his = HostedTurn {
        workspace: workspace_id,
        caller: &bobs,
        bound: None,
    };
    let open = asked(release(workspaces, &his, &wanted).await);
    assert_eq!(open.person, hex::encode(bob.user_public_key));
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}
