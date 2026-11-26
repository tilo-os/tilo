//! A secret used by reference by a turn the server carries out itself: the turn is
//! one it carries out or it is refused, the agent's own grants decide what it reaches, the first use asks its person, their
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
use crate::work::{AgentAccess, AgentEngine, Ineligible};
use crate::workspaces::fixtures::test_host;
use crate::workspaces::hosted_use::{
    CARRIED_OUT, HostedTurn, NOTHING_WAITS, OpenAsk, TurnUse, UseAnswer,
};
use crate::workspaces::tests::governor;
use crate::workspaces::{WorkspaceId, Workspaces};

const PASSWORD: &str = "correct horse battery staple";
const NAME: &str = "ada";
const IMAP: &str = "imap://imap.example.test:993";
const SMTP: &str = "smtp://smtp.example.test:465";
const MAIL_USE: &str = "tilo.accounts/add-mail-with-item";

/// The caller a turn's tools act as: `agent`, certified by `person`'s device, on a turn of
/// theirs in `workspace`.
pub(super) fn on_turn(
    workspaces: &Workspaces,
    person: &DeviceCert,
    workspace: &str,
    agent: &str,
) -> Caller {
    on_turn_for_apps(
        workspaces,
        person,
        workspace,
        agent,
        &[
            tilo_app_id::apps::BROWSER,
            tilo_app_id::apps::MAIL,
            tilo_app_id::apps::CALENDAR,
            tilo_app_id::apps::PASSWORDS,
            person_models::PROVIDER,
            tilo_actions::remote::person_connections::PROVIDER,
        ],
    )
}

/// Approve this test turn's exact signing session and app scope, as a hosted engine would.
pub(super) fn on_turn_for_apps(
    workspaces: &Workspaces,
    person: &DeviceCert,
    workspace: &str,
    agent: &str,
    apps: &[&str],
) -> Caller {
    on_turn_at_for_apps(workspaces, person, workspace, agent, 0, apps)
}

/// Approve a particular incarnation, replacing any earlier one for this agent.
pub(super) fn on_turn_at_for_apps(
    workspaces: &Workspaces,
    person: &DeviceCert,
    workspace: &str,
    agent: &str,
    created_at: i64,
    apps: &[&str],
) -> Caller {
    let workspace_id = WorkspaceId::parse(workspace).expect("a fixture workspace");
    workspaces.run_agents(Arc::new(TurnGrant {
        workspace: workspace.to_owned(),
        agent: agent.to_owned(),
        keeper: person.user_public_key,
        access: AgentAccess::Hosted {
            device: person.device_public_key,
            created_at,
            scope: tilo_server_protocol::delegation::DelegateScope {
                workspaces: vec![workspace_id],
                apps: apps.iter().map(|app| (*app).to_owned()).collect(),
            },
        },
        earlier: workspaces.agent_keys(),
    }));
    Caller::delegated_agent(agent, person.user_public_key, person.device_public_key)
        .in_workspace(workspace)
        .named("Scout")
        .in_agent_session("agent-session:test")
        .on_roster(created_at, person.user_public_key)
        .on_roster_turn(turn())
}

struct TurnGrant {
    workspace: String,
    agent: String,
    keeper: [u8; 32],
    access: AgentAccess,
    earlier: Option<Arc<dyn AgentEngine>>,
}

impl AgentEngine for TurnGrant {
    fn access<'a>(
        &'a self,
        workspace: &'a str,
        agent: &'a str,
        keeper: &'a [u8; 32],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<AgentAccess, Ineligible>> + Send + 'a>,
    > {
        if self.workspace == workspace && self.agent == agent && &self.keeper == keeper {
            return Box::pin(std::future::ready(Ok(self.access.clone())));
        }
        match &self.earlier {
            Some(engine) => engine.access(workspace, agent, keeper),
            None => Box::pin(std::future::ready(Err(Ineligible::CannotRunAgent))),
        }
    }
}

/// The turn a test's agent works.
pub(super) fn turn() -> tilo_action_policy::turn::Turn {
    tilo_action_policy::turn::Turn::new("conversation-1", "message-1")
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
        signed: None,
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
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let asker = hex::encode(ada.user_public_key);
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
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
    assert_eq!(open.person, asker);
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
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
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

/// An agent reads an item's uses with its person's access, and takes no
/// leave back: its person does, from their own device. What the item
/// remembers after.
async fn she_alone_takes_the_leave_back(
    service: &super::super::ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    ada: &DeviceCert,
    item: &Value,
) -> Value {
    let as_ada = caller(ada);
    let helper = Caller::delegated_agent("scout", ada.user_public_key, ada.device_public_key);
    ask(
        service,
        workspace_id,
        (ada, &helper),
        person_vault::USES,
        json!({"item": item}),
    )
    .await
    .expect("an agent reads what its item remembers");
    let taken = ask(
        service,
        workspace_id,
        (ada, &helper),
        person_vault::TAKE_BACK,
        json!({"item": item}),
    )
    .await;
    assert!(matches!(taken, Err(UnknownAction)), "{taken:?}");
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
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
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
    let other = on_turn(workspaces, &ada, &workspace, "other");
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
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
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

    // The server stops before the claimed answer was carried out, and starts
    // again on what it kept.
    server.shutdown().await;
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let kept = workspaces.open_use_asks(&workspace);
    assert_eq!(kept.len(), 1, "the ask is found again: {kept:?}");
    assert_eq!(kept[0].ask, open.ask);
    assert_eq!(kept[0].asked_in, json!({"part": "p0"}));
    assert!(kept[0].started && kept[0].interrupted, "{:?}", kept[0]);
    let again = workspaces.claim_use(&open.ask);
    assert!(
        matches!(&again, Err(Refused { reason }) if reason == CARRIED_OUT),
        "nobody carries it out a second time: {again:?}"
    );
    workspaces.use_interrupted(&kept[0]);
    assert!(workspaces.open_use_asks(&workspace).is_empty());
    assert!(
        proved.lock().unwrap().is_empty(),
        "nothing was signed in to"
    );
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}

/// Callers that are not a turn this server carries out for a member of the workspace: what
/// each is, and the caller.
pub(super) fn not_hosted_turns(ada: &DeviceCert, workspace: &str) -> Vec<(&'static str, Caller)> {
    let hers = hex::encode(ada.user_public_key);
    let session = || Caller::authenticated(ada.user_public_key, ada.device_public_key);
    let scout = || Caller::delegated_agent("scout", ada.user_public_key, ada.device_public_key);
    let confined_elsewhere = scout()
        .in_workspace("00000000-0000-4000-8000-000000000000")
        .on_roster(0, ada.user_public_key)
        .on_roster_turn(turn());
    let unconfined = scout()
        .on_roster(0, ada.user_public_key)
        .on_roster_turn(turn());
    let outside_a_turn = scout()
        .in_workspace(workspace)
        .on_roster(0, ada.user_public_key);
    let handed = session()
        .in_workspace(workspace)
        .delegated_to("scout")
        .in_agent_session("agent-session:test");
    let bot =
        Caller::bot("bot", ada.user_public_key, ada.device_public_key).in_workspace(workspace);
    let member = Caller::local_agent("scout").acting_for(hers.clone());
    let app = Caller::local_app("so.tilo.chat").acting_for(hers);
    vec![
        ("the person themselves", session().in_workspace(workspace)),
        ("an agent outside a turn", outside_a_turn),
        ("an agent admitted as a member", member),
        ("a bot", bot),
        ("an app", app),
        ("a turn confined to no workspace", unconfined),
        ("a turn confined to another workspace", confined_elsewhere),
        ("an agent her own device hands its session to", handed),
    ]
}

/// Every caller that is not a turn this server carries out is refused before the vault is
/// opened: a person, an agent with no turn, an agent admitted as a member, a bot, an app, a turn
/// confined elsewhere and an agent a person's own device hands its session to. Any
/// member's ask of the workspace's agent is such a turn: Bo's ask of the agent Ada asks uses the
/// same workspace item, and its use asks him, the person who asked.
#[tokio::test]
async fn any_members_turn_of_an_agent_reaches_the_workspaces_vault() {
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

    for (who, asks) in not_hosted_turns(&ada, &workspace) {
        let turn = HostedTurn {
            workspace: workspace_id,
            caller: &asks,
            bound: None,
        };
        let refused = workspaces.use_for_turn(&turn, &adding, json!({})).await;
        assert_eq!(refused, Err(UnknownAction), "{who}");
        assert!(workspaces.uses_offered(&turn).await.is_empty(), "{who}");
    }

    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");
    assert!(
        workspaces.open_use_asks(&workspace).is_empty(),
        "nobody was asked"
    );

    // The same agent, asked by Ada and by Bo, is offered the same uses and reaches the same
    // item: each ask is put to whoever asked.
    let ada_id = hex::encode(ada.user_public_key);
    let bo_id = hex::encode(bo.user_public_key);
    let for_ada = on_turn(workspaces, &ada, &workspace, "scout");
    let for_bo = on_turn(workspaces, &bo, &workspace, "scout");
    let (hers, his) = (
        HostedTurn {
            workspace: workspace_id,
            caller: &for_ada,
            bound: None,
        },
        HostedTurn {
            workspace: workspace_id,
            caller: &for_bo,
            bound: None,
        },
    );
    assert_eq!(
        workspaces.uses_offered(&hers).await,
        workspaces.uses_offered(&his).await
    );
    let open = asked(workspaces.use_for_turn(&his, &adding, json!({})).await);
    assert_eq!(
        open.person, bo_id,
        "the workspace's item asks whoever asked"
    );
    assert_eq!(open.agent, "scout");
    let open = asked(workspaces.use_for_turn(&hers, &adding, json!({})).await);
    assert_eq!(open.person, ada_id);
    assert_eq!(open.agent, "scout");

    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");
    assert_eq!(workspaces.open_use_asks(&workspace).len(), 2);
    assert_eq!(
        remembered(&service, workspace_id, (&ada, &as_ada), &login).await,
        json!({"uses": []})
    );
    server.shutdown().await;
}

/// What reaches the workspace's vault is the agent's own word in the workspace's policy, not
/// the asker's: an owner's grant that keeps the agent from the vault refuses its turn though
/// Ada, who asked it, keeps and reads the item herself, and once the grant is gone the same
/// turn reaches it again. Another agent Ada asks is not held by that grant.
#[tokio::test]
async fn the_agents_own_grants_decide_what_its_turn_reaches_of_the_vault() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let login = a_mail_login(&service, workspace_id, (&ada, &as_ada)).await;
    let adding = add_mail("ada@example.test", "imap.example.test", &login);
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let other = on_turn(workspaces, &ada, &workspace, "other");
    let turn = |caller| HostedTurn {
        workspace: workspace_id,
        caller,
        bound: None,
    };

    let kept_from_vault = tilo_action_policy::Grant::new(
        "test:scout-kept-from-vault",
        tilo_action_policy::Grantee::agent("scout"),
        TreePath::workspace(&workspace)
            .app(tilo_app_id::apps::PASSWORDS)
            .kind("passwords.workspace-vault"),
        KindSet::ALL,
        Answer::Deny,
        tilo_action_policy::Grantee::person(hex::encode(owner.user_public_key)),
    );
    workspaces
        .put_workspace_grants(&workspace, &owner, [61; 32], vec![kept_from_vault])
        .await
        .unwrap();
    assert_eq!(
        workspaces
            .use_for_turn(&turn(&scout), &adding, json!({}))
            .await,
        Err(NotPermitted),
        "the agent is kept from the vault"
    );
    assert!(workspaces.uses_offered(&turn(&scout)).await.is_empty());
    assert!(
        remembered(&service, workspace_id, (&ada, &as_ada), &login).await["uses"].is_array(),
        "Ada still reads the item herself"
    );
    let open = asked(
        workspaces
            .use_for_turn(&turn(&other), &adding, json!({}))
            .await,
    );
    assert_eq!(
        open.agent, "other",
        "another agent is not held by the grant"
    );

    workspaces
        .remove_workspace_grants(
            &workspace,
            &owner,
            [62; 32],
            vec![tilo_action_policy::GrantId(
                "test:scout-kept-from-vault".into(),
            )],
        )
        .await
        .unwrap();
    let open = asked(
        workspaces
            .use_for_turn(&turn(&scout), &adding, json!({}))
            .await,
    );
    assert_eq!(
        open.agent, "scout",
        "without the grant the agent reaches the item"
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
    let scout = on_turn(workspaces, &ada, &workspace, "sub-agent");
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
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
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
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
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
    // Both turns kept the key, and the host then called the provider with it: the entry is
    // read where it is, and that call is a use of it too.
    assert_eq!(kept["uses"].as_array().unwrap().len(), 3, "{kept}");
    assert_eq!(kept["uses"][0]["allowed"], "always", "{kept}");
    assert_eq!(kept["uses"][1]["allowed"], "always", "{kept}");
    assert_eq!(kept["uses"][2]["using"], "model-call", "{kept}");
    assert_eq!(
        kept["uses"][2]["places"],
        json!(["https://models.example"]),
        "{kept}"
    );
    assert_eq!(kept["always"][0]["using"], "tilo.models/set-key-from-item");
    server.shutdown().await;
}

fn key_use_scope(
    open: &OpenAsk,
    item: &person_vault::ItemRef,
) -> tilo_actions::remote::person_asking::Asked {
    use tilo_actions::remote::person_asking::{ActionName, Asked, Thing};

    let target = open.target();
    Asked {
        workspace: open.workspace.clone(),
        agent: open.agent.clone(),
        action: ActionName {
            kind: person_vault::use_word(&target),
            provider: target.provider,
            action: target.address.action,
        },
        on: Some(Thing {
            document: item.id().to_owned(),
            resource: open.detail["use"]["item"]["belongsTo"].to_string(),
        }),
    }
}

#[tokio::test]
async fn finishing_a_hosted_key_use_requires_current_store_and_item_permissions() {
    use crate::workspaces::workspace_stores::Store;
    use accounts::vault_items::{Allowance, UsedFor};
    use tilo_action_policy::{Grant, Grantee};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let provider = test_http::Responder::handle(|_| {
        Some(test_http::Reply::status(200).body(r#"{"data":[{"id":"test-model"}]}"#))
    });
    workspaces.call_private_model_addresses(true);
    let added = ask_at(
        &service,
        id,
        hers,
        person_models::target(person_models::ADD_PROVIDER),
        json!({"name": "Test provider", "protocol": "open-ai-compatible", "endpoint": provider.url()}),
    )
    .await
    .unwrap();
    let item = kept_item(&service, id, hers, ("key", &provider.url())).await;
    let scout = on_turn(workspaces, &ada, &workspace, "scout");
    let turn = HostedTurn {
        workspace: id,
        caller: &scout,
        bound: None,
    };
    let setting = person_models::SetKeyFromItem {
        provider: added["id"].as_str().unwrap().to_owned(),
        key: serde_json::from_value(item.clone()).unwrap(),
        reason: String::new(),
    };
    let keeping = request(
        person_models::target(person_models::SET_KEY_FROM_ITEM),
        serde_json::to_value(&setting).unwrap(),
    );
    let open = asked(workspaces.use_for_turn(&turn, &keeping, json!({})).await);
    let scope = key_use_scope(&open, &setting.key);
    let check = |allowed| workspaces.may_finish_key_use(&turn, &scope, allowed);
    let kept = workspaces
        .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Always)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept["configured"], true);
    assert_eq!(check(Allowance::Always).await, Ok(()));
    let removed = ask_at(
        &service,
        id,
        hers,
        person_models::target(person_models::REMOVE_KEY),
        json!({"provider": added["id"]}),
    )
    .await
    .unwrap();
    assert_eq!(removed["configured"], false);

    she_alone_takes_the_leave_back(&service, id, &ada, &item).await;
    assert_eq!(check(Allowance::Always).await, Err(NotPermitted));
    assert_eq!(check(Allowance::Once).await, Ok(()));
    let root = workspaces.workspace_root(&workspace);
    let refused = workspaces
        .key_from_item(
            (&workspace, &root),
            &setting,
            &open.name,
            UsedFor {
                using: &scope.action.kind,
                what: "",
                allowed: Some(Allowance::Always),
            },
            std::future::ready(Err(NotPermitted)),
        )
        .await;
    assert!(matches!(refused, Err(NotPermitted)));

    for (store, put, remove) in [(Store::VAULT, 41, 42), (Store::MODELS, 43, 44)] {
        let open = asked(workspaces.use_for_turn(&turn, &keeping, json!({})).await);
        let deny = Grant::new(
            "deny-scout-key-use",
            Grantee::agent("scout"),
            store.node(&workspace, person_models::SET_KEY_FROM_ITEM),
            KindSet::UNDOABLE,
            Answer::Deny,
            Grantee::person(hex::encode(owner.user_public_key)),
        );
        let grant_id = deny.id.clone();
        workspaces
            .put_workspace_grants(&workspace, &owner, [put; 32], vec![deny])
            .await
            .unwrap();
        assert_eq!(check(Allowance::Once).await, Err(NotPermitted), "{store:?}");
        assert_eq!(
            workspaces
                .answer_use(&turn, &open.ask, &open.detail, UseAnswer::Once)
                .await,
            Err(NotPermitted),
            "{store:?}"
        );
        workspaces
            .remove_workspace_grants(&workspace, &owner, [remove; 32], vec![grant_id])
            .await
            .unwrap();
    }
    let models = ask_at(
        &service,
        id,
        hers,
        person_models::target(person_models::LIST),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(models["providers"][0]["id"], added["id"]);
    assert_eq!(models["providers"][0]["configured"], false);
    server.shutdown().await;
}
