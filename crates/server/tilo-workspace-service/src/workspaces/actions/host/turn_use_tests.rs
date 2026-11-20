//! A secret used by reference by a turn the server carries out itself: the turn is
//! its keeper's own or it is refused, the first use asks its person, their
//! "Always" lets that agent use that item there without asking, and they
//! read that leave and take it back.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, and the server handle a test binds is kept until the test ends"
)]

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_action_policy::{ActionKind, Answer, KindSet, Level, Rule, TreePath};
use tilo_actions::InvocationId;
use tilo_actions::remote::ActionCallError::{
    self, InvalidRequest, NotPermitted, Refused, UnknownAction,
};
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_actions::remote::{person_accounts, person_models, person_vault};
use tilo_user_identity::DeviceCert;

use super::super::tests::{admit, caller, governed};
use super::vault_tests::{a_provider, ask, ask_at, files_holding, kept_item};
use crate::workspaces::hosted_use::{
    CARRIED_OUT, HostedTurn, NOTHING_WAITS, OpenAsk, TurnUse, UseAnswer,
};
use crate::workspaces::tests::{governor, test_host};
use crate::workspaces::{WorkspaceId, Workspaces};

const PASSWORD: &str = "correct horse battery staple";
const NAME: &str = "ada";
const IMAP: &str = "imap://imap.example.test:993";
const SMTP: &str = "smtp://smtp.example.test:465";
const MAIL_USE: &str = "tilo.accounts/add-mail-with-item";

/// The caller a turn's tools act as: `person`'s session in `workspace`,
/// handed to `agent` on a turn.
pub(super) fn on_turn(person: &DeviceCert, workspace: &str, agent: &str) -> Caller {
    Caller::authenticated(person.user_public_key, person.device_public_key)
        .in_workspace(workspace)
        .delegated_to(agent)
        .named("Scout")
        .in_agent_session("agent-session:test")
        .on_turn(
            tilo_action_policy::turn::Turn::new("conversation-1", "message-1"),
            Vec::new(),
        )
}

fn request(target: RemoteActionTarget, argument: Value) -> InvokeRequest {
    let Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

/// Adding the account `address` at the servers the login belongs to, or at
/// `incoming` instead, with the vault item `password` names.
pub(super) fn add_mail(address: &str, incoming: &str, password: &Value) -> InvokeRequest {
    request(
        person_accounts::target(person_accounts::ADD_MAIL_WITH_ITEM),
        json!({
            "address": address,
            "incomingHost": incoming,
            "outgoingHost": "smtp.example.test",
            "password": password,
            "reason": "You asked me to read your mail.",
        }),
    )
}

/// A login for both mail servers, kept by `who` from their own device.
pub(super) async fn a_mail_login(
    service: &super::super::ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> Value {
    let kept = ask(
        service,
        workspace_id,
        who,
        person_vault::ADD,
        json!({"kind": "login", "belongsTo": [IMAP, SMTP], "username": NAME, "secret": PASSWORD}),
    )
    .await
    .expect("the person keeps a login");
    kept["reference"].clone()
}

/// Sign-ins are proved by a stand-in that accepts each and keeps what it
/// was asked with.
pub(super) fn proving(workspaces: &Workspaces) -> Arc<Mutex<Vec<String>>> {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&asked);
    workspaces.sign_in_accounts_through(move |account, password| {
        kept.lock()
            .unwrap()
            .push(format!("{account} with {password}"));
        Ok("signed in".into())
    });
    asked
}

/// What `item` remembers, as its person reads it from their own device.
async fn remembered(
    service: &super::super::ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    item: &Value,
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

/// The ask a use answered with, when it asked.
pub(super) fn asked(taken: Result<TurnUse, ActionCallError>) -> OpenAsk {
    match taken {
        Ok(TurnUse::Asks(open)) => *open,
        other => panic!("the person is asked first: {other:?}"),
    }
}

/// The first use of an item by a turn the server carries out asks its person and
/// uses nothing. What is kept for the question holds the reference and no
/// secret, the same use asked again does not ask twice, and a question
/// that shows something else than the server asked, or a "no", uses nothing.
#[tokio::test]
async fn the_first_use_by_a_hosted_turn_asks_its_person_and_uses_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let login = a_mail_login(&service, workspace_id, hers).await;
    let proved = proving(workspaces);
    let scout = on_turn(&ada, &workspace, "scout");
    let keeper = hex::encode(ada.user_public_key);
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        keeper: &keeper,
        bound: None,
    };
    assert_eq!(workspaces.uses_offered(&turn).await.len(), 2);

    let adding = add_mail("ada@example.test", "imap.example.test", &login);
    let open = asked(
        workspaces
            .use_for_turn(&turn, &adding, json!({"part": "p0"}))
            .await,
    );
    assert_eq!(open.agent, "scout");
    assert_eq!(open.name, "Scout");
    assert_eq!(open.person, keeper);
    assert_eq!(open.workspace, workspace);
    assert_eq!(open.reason, "You asked me to read your mail.");
    assert_eq!(open.asked_in, json!({"part": "p0"}));
    assert_eq!(open.detail["use"]["ask"], json!(open.ask));
    assert_eq!(
        open.detail["use"]["item"],
        json!({"kind": "login", "belongsTo": [IMAP, SMTP]})
    );
    // What the person is shown says where it signs in and withholds the item.
    let shown = open.detail.to_string();
    assert!(shown.contains("ada@example.test"), "{shown}");
    let id = login.as_str().unwrap().trim_start_matches("vault.item:");
    assert!(!shown.contains(id) && !shown.contains(PASSWORD), "{shown}");
    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");
    assert_eq!(
        remembered(&service, workspace_id, hers, &login).await,
        json!({"uses": []})
    );

    // Asked the same again, the one question stands.
    let again = add_mail("ada@example.test", "imap.example.test", &login);
    assert_eq!(
        workspaces.use_for_turn(&turn, &again, json!({})).await,
        Ok(TurnUse::Waits)
    );
    assert_eq!(workspaces.open_use_asks(&workspace), vec![open.clone()]);
    assert!(workspaces.open_use_asks("another-workspace").is_empty());

    // A question that shows something else than this server asked uses
    // nothing, and the ask is spent with it.
    let mut other = open.detail.clone();
    other["use"]["item"]["belongsTo"] = json!(["imap://imap.attacker.example:993"]);
    let refused = workspaces
        .answer_use(&turn, &open.ask, &other, UseAnswer::Always)
        .await;
    assert!(
        matches!(&refused, Err(Refused { reason }) if reason.contains("not the one this server asked")),
        "{refused:?}"
    );
    let spent = workspaces
        .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Once)
        .await;
    assert!(
        matches!(&spent, Err(Refused { reason }) if reason.contains("Nothing waits")),
        "{spent:?}"
    );

    // A "no" uses nothing either.
    let open = asked(workspaces.use_for_turn(&turn, &adding, json!({})).await);
    assert_eq!(
        workspaces
            .answer_use(&turn, &open.ask, &open.detail, UseAnswer::No)
            .await,
        Ok(None)
    );
    assert!(workspaces.open_use_asks(&workspace).is_empty());
    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");
    let remembers = remembered(&service, workspace_id, hers, &login).await;
    assert_eq!(remembers, json!({"uses": []}), "no use and no leave");
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}

/// Allowed once, a use is made once: it signs in with what the item keeps,
/// answers the account and never the password, is written down, and leaves
/// no leave behind, so the next use asks again.
#[tokio::test]
async fn allowed_once_the_use_is_made_once_and_leaves_no_leave() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let login = a_mail_login(&service, workspace_id, hers).await;
    let proved = proving(workspaces);
    let scout = on_turn(&ada, &workspace, "scout");
    let keeper = hex::encode(ada.user_public_key);
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        keeper: &keeper,
        bound: None,
    };
    let add = |address: &str| add_mail(address, "imap.example.test", &login);

    let wanted = add("ada@example.test");
    let first = workspaces.use_for_turn(&turn, &wanted, json!({})).await;
    let open = asked(first);
    let account = workspaces
        .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Once)
        .await
        .expect("allowed once, it is made")
        .expect("it answers the account");
    assert_eq!(account["address"], "ada@example.test");
    assert!(!account.to_string().contains(PASSWORD), "{account}");
    assert_eq!(
        proved.lock().unwrap().clone(),
        vec![format!("ada@example.test with {PASSWORD}")]
    );
    let twice = workspaces
        .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Once)
        .await;
    assert!(matches!(twice, Err(Refused { .. })), "{twice:?}");
    let remembers = remembered(&service, workspace_id, hers, &login).await;
    assert_eq!(
        remembers["uses"].as_array().unwrap().len(),
        1,
        "{remembers}"
    );
    assert_eq!(remembers["uses"][0]["by"], "Scout");
    assert_eq!(remembers["uses"][0]["using"], MAIL_USE);
    assert_eq!(remembers["uses"][0]["allowed"], "once");
    assert_eq!(remembers.get("always"), None, "once leaves no leave");

    // The next account asks again.
    let wanted = add("news@example.test");
    let next = workspaces.use_for_turn(&turn, &wanted, json!({})).await;
    asked(next);
    assert_eq!(proved.lock().unwrap().len(), 1, "nothing more signed in");
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}

/// No agent reads an item's leaves or takes them back. Its person does,
/// from their own device: what the item remembers after.
async fn she_alone_takes_the_leave_back(
    service: &super::super::ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    ada: &DeviceCert,
    item: &Value,
) -> Value {
    let as_ada = caller(ada);
    let helper = Caller::delegated_agent("scout", ada.user_public_key, ada.device_public_key);
    for action in [person_vault::USES, person_vault::TAKE_BACK] {
        let refused = ask(
            service,
            workspace_id,
            (ada, &helper),
            action,
            json!({"item": item}),
        )
        .await;
        assert!(
            matches!(refused, Err(UnknownAction)),
            "{action}: {refused:?}"
        );
    }
    ask(
        service,
        workspace_id,
        (ada, &as_ada),
        person_vault::TAKE_BACK,
        json!({"item": item}),
    )
    .await
    .expect("she takes the leave back")
}

/// After "Always" the same agent uses the same item at the same servers
/// without asking, each use written down, while another agent of hers is
/// still asked. The person reads the leave and takes it back from their own
/// device, which no agent does, and then the next use asks again.
#[tokio::test]
async fn after_always_the_agent_uses_the_item_there_alone_until_it_is_taken_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let login = a_mail_login(&service, workspace_id, hers).await;
    let proved = proving(workspaces);
    let scout = on_turn(&ada, &workspace, "scout");
    let keeper = hex::encode(ada.user_public_key);
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        keeper: &keeper,
        bound: None,
    };
    let add = |address: &str| add_mail(address, "imap.example.test", &login);

    // "Always" writes the leave and makes the use.
    let wanted = add("news@example.test");
    let first = workspaces.use_for_turn(&turn, &wanted, json!({})).await;
    let open = asked(first);
    workspaces
        .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Always)
        .await
        .expect("allowed always, it is made")
        .expect("it answers the account");
    let remembers = remembered(&service, workspace_id, hers, &login).await;
    assert_eq!(
        remembers["always"],
        json!([{
            "agent": "scout",
            "using": MAIL_USE,
            "places": [IMAP, SMTP],
            "since": remembers["always"][0]["since"],
        }])
    );
    assert!(
        remembers["always"][0]["since"]
            .as_u64()
            .is_some_and(|since| since > 0)
    );

    // From now on that agent uses that item there without a question, and
    // each use is written down.
    let second = workspaces
        .use_for_turn(&turn, &add("work@example.test"), json!({}))
        .await;
    assert!(
        matches!(&second, Ok(TurnUse::Done(account)) if account["address"] == "work@example.test"),
        "{second:?}"
    );
    assert!(workspaces.open_use_asks(&workspace).is_empty());
    assert_eq!(proved.lock().unwrap().len(), 2);
    let remembers = remembered(&service, workspace_id, hers, &login).await;
    assert_eq!(
        remembers["uses"].as_array().unwrap().len(),
        2,
        "{remembers}"
    );
    // Both were allowed by the leave: the one her "Always" answered, and
    // the one made alone under it.
    for used in remembers["uses"].as_array().unwrap() {
        assert_eq!(used["using"], MAIL_USE, "{remembers}");
        assert_eq!(used["allowed"], "always", "{remembers}");
    }

    // Another agent of hers is still asked: the leave names one agent.
    let other = on_turn(&ada, &workspace, "other");
    let others = HostedTurn {
        caller: &other,
        ..turn
    };
    let wanted = add("more@example.test");
    let theirs = workspaces.use_for_turn(&others, &wanted, json!({})).await;
    asked(theirs);

    // She takes the leave back, and then her agent is asked again.
    let taken = she_alone_takes_the_leave_back(&service, workspace_id, &ada, &login).await;
    assert_eq!(taken.get("always"), None, "{taken}");
    assert_eq!(taken["uses"].as_array().unwrap().len(), 2);
    let wanted = add("late@example.test");
    let late = workspaces.use_for_turn(&turn, &wanted, json!({})).await;
    asked(late);
    assert_eq!(proved.lock().unwrap().len(), 2, "nothing more signed in");
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}

/// An answer is carried out by the one reader that claimed it. A server that
/// stops while it carries one out finds the ask started when it is back:
/// nobody claims it again, whoever carries out the agents' turns reads that it was
/// interrupted, and nothing was signed in to.
#[tokio::test]
async fn an_answer_is_claimed_once_and_not_carried_out_again_after_the_server_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let login = a_mail_login(&service, workspace_id, (&ada, &as_ada)).await;
    let proved = proving(workspaces);
    let scout = on_turn(&ada, &workspace, "scout");
    let keeper = hex::encode(ada.user_public_key);
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        keeper: &keeper,
        bound: None,
    };
    let adding = add_mail("ada@example.test", "imap.example.test", &login);
    let open = asked(
        workspaces
            .use_for_turn(&turn, &adding, json!({"part": "p0"}))
            .await,
    );

    let claimed = workspaces
        .claim_use(&open.ask)
        .expect("the first reader of the answer claims it");
    assert!(claimed.started && !claimed.interrupted);
    let second = workspaces.claim_use(&open.ask);
    assert!(
        matches!(&second, Err(Refused { reason }) if reason == CARRIED_OUT),
        "{second:?}"
    );
    let unknown = workspaces.claim_use("0123456789abcdef0123456789abcdef");
    assert!(
        matches!(&unknown, Err(Refused { reason }) if reason == NOTHING_WAITS),
        "{unknown:?}"
    );
    // While it is claimed, the same use asked again puts no second question.
    assert_eq!(
        workspaces.use_for_turn(&turn, &adding, json!({})).await,
        Ok(TurnUse::Waits)
    );

    // Pending vault asks are in-memory only and cleared on restart.
    server.shutdown().await;
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    assert!(workspaces.open_use_asks(&workspace).is_empty());
    assert!(
        proved.lock().unwrap().is_empty(),
        "nothing was signed in to"
    );
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}

/// Callers that are not a turn of Ada's own agent for her: what each is,
/// the caller, and whom the agent is said to be kept by.
pub(super) fn not_her_agents_turns(
    ada: &DeviceCert,
    bo: &DeviceCert,
    workspace: &str,
) -> Vec<(&'static str, Caller, String)> {
    let hers = hex::encode(ada.user_public_key);
    let his = hex::encode(bo.user_public_key);
    let session = || Caller::authenticated(ada.user_public_key, ada.device_public_key);
    let turn = || tilo_action_policy::turn::Turn::new("conversation-1", "message-1");
    let on_another_device = Caller::authenticated(ada.user_public_key, [7; 32])
        .in_workspace(workspace)
        .delegated_to("scout")
        .on_turn(turn(), Vec::new());
    let confined_elsewhere = session()
        .in_workspace("00000000-0000-4000-8000-000000000000")
        .delegated_to("scout")
        .on_turn(turn(), Vec::new());
    let unconfined = session().delegated_to("scout").on_turn(turn(), Vec::new());
    let outside_a_turn = session().in_workspace(workspace).delegated_to("scout");
    let bot =
        Caller::bot("bot", ada.user_public_key, ada.device_public_key).in_workspace(workspace);
    let member = Caller::local_agent("scout").acting_for(hers.clone());
    let app = Caller::local_app("so.tilo.chat").acting_for(hers.clone());
    vec![
        (
            "the person themselves",
            session().in_workspace(workspace),
            hers.clone(),
        ),
        ("an agent outside a turn", outside_a_turn, hers.clone()),
        ("an agent admitted as a member", member, hers.clone()),
        ("a bot", bot, hers.clone()),
        ("an app", app, hers.clone()),
        ("a turn confined to no workspace", unconfined, hers.clone()),
        (
            "a turn confined to another workspace",
            confined_elsewhere,
            hers.clone(),
        ),
        (
            "a turn on a device she is not admitted on",
            on_another_device,
            hers.clone(),
        ),
        // Bo wrote to her agent: the turn acts for Bo, and the agent is hers.
        (
            "another member's ask of her agent",
            on_turn(bo, workspace, "scout"),
            hers,
        ),
        (
            "her turn for an agent somebody else keeps",
            on_turn(ada, workspace, "scout"),
            his,
        ),
        (
            "her turn for an agent nobody keeps",
            on_turn(ada, workspace, "scout"),
            String::new(),
        ),
    ]
}

/// Every caller that is not an agent's turn for its own keeper is refused
/// before the vault is opened: a person, an agent with no turn, an agent
/// admitted as a member, a bot, an app, a turn confined elsewhere, a turn
/// on a device the workspace does not admit that person on, and another
/// member's ask of the agent. Another member's own agent finds nothing
/// under her reference.
#[tokio::test]
async fn only_an_agents_turn_for_its_own_keeper_uses_their_vault() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let login = a_mail_login(&service, workspace_id, hers).await;
    let proved = proving(workspaces);
    let adding = add_mail("ada@example.test", "imap.example.test", &login);

    for (who, asks, keeper) in not_her_agents_turns(&ada, &bo, &workspace) {
        let turn = HostedTurn {
            workspace: workspace_id,
            caller: &asks,
            keeper: &keeper,
            bound: None,
        };
        let refused = workspaces.use_for_turn(&turn, &adding, json!({})).await;
        assert_eq!(refused, Err(UnknownAction), "{who}");
        assert!(workspaces.uses_offered(&turn).await.is_empty(), "{who}");
    }

    // Bo's own agent, on Bo's own turn, is not refused as a turn. Her
    // reference names nothing in his vault, which is the only one opened.
    let bo_id = hex::encode(bo.user_public_key);
    let bos = on_turn(&bo, &workspace, "bos-agent");
    let his = HostedTurn {
        workspace: workspace_id,
        caller: &bos,
        keeper: &bo_id,
        bound: None,
    };
    let nothing = workspaces.use_for_turn(&his, &adding, json!({})).await;
    assert!(
        matches!(&nothing, Err(Refused { reason }) if reason == "That item is not kept here"),
        "{nothing:?}"
    );

    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");
    assert!(
        workspaces.open_use_asks(&workspace).is_empty(),
        "nobody was asked"
    );
    assert_eq!(
        remembered(&service, workspace_id, hers, &login).await,
        json!({"uses": []})
    );
    server.shutdown().await;
}

/// An agent its entry narrows uses an item only where a rule of its bound
/// names exactly this use: no rule, a rule for the whole provider, a rule
/// for another action and a rule that denies it all refuse. With the rule,
/// it asks its person first like any agent.
#[tokio::test]
async fn a_narrowed_agent_needs_a_rule_that_names_the_use() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let login = a_mail_login(&service, workspace_id, (&ada, &as_ada)).await;
    let proved = proving(workspaces);
    let scout = on_turn(&ada, &workspace, "sub-agent");
    let keeper = hex::encode(ada.user_public_key);
    let adding = add_mail("ada@example.test", "imap.example.test", &login);
    let outside = KindSet::of(ActionKind::Outside);
    let named = |action: &str, answer: Answer| {
        tilo_automation::action_rule(
            person_accounts::PROVIDER,
            person_accounts::RESOURCE,
            None,
            action,
            outside,
            answer,
        )
    };
    let whole_provider = Rule::new(
        TreePath::root()
            .any(Level::Workspace)
            .app(person_accounts::PROVIDER),
        outside,
        Answer::Allow,
    );

    for (why, bound) in [
        ("no rule at all", Vec::new()),
        (
            "a rule for the whole provider",
            vec![whole_provider.clone()],
        ),
        (
            "a rule for another action",
            vec![named(person_accounts::LIST, Answer::Allow)],
        ),
        (
            "a rule that denies the use",
            vec![
                whole_provider,
                named(person_accounts::ADD_MAIL_WITH_ITEM, Answer::Deny),
            ],
        ),
    ] {
        let turn = HostedTurn {
            workspace: workspace_id,
            caller: &scout,
            keeper: &keeper,
            bound: Some(&bound),
        };
        let refused = workspaces.use_for_turn(&turn, &adding, json!({})).await;
        assert_eq!(refused, Err(NotPermitted), "{why}");
        assert!(
            workspaces.uses_offered(&turn).await.is_empty(),
            "{why}: it is not offered what it would be refused"
        );
    }
    assert!(
        workspaces.open_use_asks(&workspace).is_empty(),
        "nobody was asked"
    );

    let bound = vec![named(person_accounts::ADD_MAIL_WITH_ITEM, Answer::Allow)];
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        keeper: &keeper,
        bound: Some(&bound),
    };
    let offered = workspaces.uses_offered(&turn).await;
    assert_eq!(offered.len(), 1, "the one use its bound names");
    asked(workspaces.use_for_turn(&turn, &adding, json!({})).await);
    assert!(
        proved.lock().unwrap().is_empty(),
        "a rule of its bound is no leave of its person's: it still asks"
    );
    server.shutdown().await;
}

/// The vault's own refusals come before anyone is asked, in its own words:
/// a server the item does not belong to, another kind than the use needs,
/// a reference that names nothing, and arguments that do not read, which
/// are never repeated.
#[tokio::test]
async fn a_use_the_vault_refuses_asks_nobody() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let login = a_mail_login(&service, workspace_id, hers).await;
    let provider = a_provider(&service, workspace_id, hers).await;
    let api_key = kept_item(
        &service,
        workspace_id,
        hers,
        ("key", "https://models.example"),
    )
    .await;
    let proved = proving(workspaces);
    let scout = on_turn(&ada, &workspace, "scout");
    let keeper = hex::encode(ada.user_public_key);
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        keeper: &keeper,
        bound: None,
    };
    let unknown = json!("vault.item:0123456789abcdef0123456789abcdef");

    for (asking, words) in [
        (
            add_mail("ada@example.test", "imap.attacker.example", &login),
            "That item belongs somewhere else",
        ),
        (
            add_mail("ada@example.test", "imap.example.test", &api_key),
            "That item belongs somewhere else",
        ),
        (
            add_mail("ada@example.test", "imap.example.test", &unknown),
            "That item is not kept here",
        ),
    ] {
        let refused = workspaces.use_for_turn(&turn, &asking, json!({})).await;
        assert!(
            matches!(&refused, Err(Refused { reason }) if reason == words),
            "{words}: {refused:?}"
        );
    }
    let as_key = request(
        person_models::target(person_models::SET_KEY_FROM_ITEM),
        json!({"provider": provider, "key": login}),
    );
    let wrong_kind = workspaces.use_for_turn(&turn, &as_key, json!({})).await;
    assert!(
        matches!(&wrong_kind, Err(Refused { .. })),
        "a login is no key: {wrong_kind:?}"
    );
    // A password typed where the reference belongs is refused unread.
    let typed = add_mail("ada@example.test", "imap.example.test", &json!(PASSWORD));
    let unread = workspaces.use_for_turn(&turn, &typed, json!({})).await;
    assert!(
        matches!(&unread, Err(InvalidRequest { reason }) if !reason.contains(PASSWORD)),
        "{unread:?}"
    );
    let other = request(person_accounts::target(person_accounts::LIST), json!({}));
    assert_eq!(
        workspaces.use_for_turn(&turn, &other, json!({})).await,
        Err(UnknownAction),
        "only the uses by reference are taken here"
    );
    assert!(
        workspaces.open_use_asks(&workspace).is_empty(),
        "nobody was asked"
    );
    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");
    server.shutdown().await;
}

/// A provider's key goes the same way as a mail sign-in: asked for first,
/// with the question saying which key and where, and after "Always" kept
/// without a question, each use written down.
#[tokio::test]
async fn a_providers_key_is_asked_for_first_and_then_kept_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let provider = a_provider(&service, workspace_id, hers).await;
    let api_key = kept_item(
        &service,
        workspace_id,
        hers,
        ("key", "https://models.example"),
    )
    .await;
    let scout = on_turn(&ada, &workspace, "scout");
    let keeper = hex::encode(ada.user_public_key);
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        keeper: &keeper,
        bound: None,
    };
    let keeping = || {
        request(
            person_models::target(person_models::SET_KEY_FROM_ITEM),
            json!({"provider": provider, "key": api_key}),
        )
    };

    let open = asked(workspaces.use_for_turn(&turn, &keeping(), json!({})).await);
    assert_eq!(
        open.detail["use"]["item"],
        json!({"kind": "key", "belongsTo": ["https://models.example"]})
    );
    let view = workspaces
        .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Always)
        .await
        .expect("allowed always, the key is kept")
        .expect("it answers the provider");
    assert_eq!(view["configured"], json!(true), "{view}");
    let alone = workspaces.use_for_turn(&turn, &keeping(), json!({})).await;
    assert!(matches!(alone, Ok(TurnUse::Done(_))), "{alone:?}");
    assert!(workspaces.open_use_asks(&workspace).is_empty());
    let kept = ask_at(
        &service,
        workspace_id,
        hers,
        person_vault::target(person_vault::USES),
        json!({"item": api_key}),
    )
    .await
    .unwrap();
    assert_eq!(kept["uses"].as_array().unwrap().len(), 2, "{kept}");
    assert_eq!(kept["uses"][0]["allowed"], "always", "{kept}");
    assert_eq!(kept["uses"][1]["allowed"], "always", "{kept}");
    assert_eq!(kept["always"][0]["using"], "tilo.models/set-key-from-item");
    server.shutdown().await;
}
