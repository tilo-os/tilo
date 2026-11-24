//! A secret used by reference by a turn the server carries out itself: the turn is one it
//! carries out or it is refused, the agent's own grants decide what it reaches, a use no grant
//! covers waits for its person's yes and runs once on their approve, the grant their "always"
//! gives lets that agent use that item there without asking, and they read that grant and take
//! it back.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, and the server handle a test binds is kept until the test ends"
)]

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tilo_action_policy::{ActionKind, Answer, Grant, Grantee, KindSet, Rule, TreePath};
use tilo_actions::InvocationId;
#[cfg(feature = "agents")]
use tilo_actions::remote::ActionCallError::NotPermitted;
use tilo_actions::remote::ActionCallError::{self, InvalidRequest, Refused, UnknownAction};
use tilo_actions::remote::asked::{AskedBy, AskedFacts, Named};
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_actions::remote::{model_providers, person_accounts, person_vault};
use tilo_user_identity::DeviceCert;

use super::super::tests::{admit, caller, governed};
use super::vault_tests::{a_provider, ask, ask_at, files_holding, kept_item};
use crate::workspaces::fixtures::test_host;
use crate::workspaces::hosted_use::HostedTurn;
use crate::workspaces::tests::governor;
use crate::workspaces::{WorkspaceId, Workspaces};

const PASSWORD: &str = "correct horse battery staple";
const NAME: &str = "ada";
const IMAP: &str = "imap://imap.example.test:993";
const SMTP: &str = "smtp://smtp.example.test:465";
const MAIL_USE: &str = "tilo.accounts/add-mail-with-item";

/// The caller a turn's tools act as: `agent`, certified by `person`'s device, on a turn of
/// theirs in `workspace`.
pub(super) fn on_turn(person: &DeviceCert, workspace: &str, agent: &str) -> Caller {
    Caller::delegated_agent(agent, person.user_public_key, person.device_public_key)
        .in_workspace(workspace)
        .named("Scout")
        .in_agent_session("agent-session:test")
        .on_roster(0, person.user_public_key)
        .on_roster_turn(turn())
}

/// The turn a test's agent works.
pub(super) fn turn() -> tilo_action_policy::turn::Turn {
    tilo_action_policy::turn::Turn::new("conversation-1", "message-1")
}

/// `request`, asked by `turn`'s caller of the device's dispatch, as a turn's tools ask it: what
/// it answered, or its refusal. An agent on a turn in the turn's workspace is listed there
/// first, and where the turn names a policy, its person's answers in it replace whatever the
/// workspace held for the agent, as setting them anew does. An agent another of the person's
/// devices hands its session to is no turn this device carries out, and is answered as the
/// device answers that call for any caller.
pub(super) async fn dispatched(
    workspaces: &Workspaces,
    turn: &HostedTurn<'_>,
    request: &InvokeRequest,
) -> Result<Value, ActionCallError> {
    let caller = turn.caller;
    let workspace = turn.workspace.to_string();
    if crate::workspaces::hosted_use::answers(caller, &request.target)
        && caller.workspace() == Some(workspace.as_str())
        && let (Some(agent), Some(person)) = (caller.acting().agent_id(), caller.turn_person())
    {
        listed(workspaces, &workspace, &hex::encode(person), agent, &[]).await;
        // A turn given no policy leaves the workspace's grants as the test set them.
        if let Some(policy) = turn.policy {
            let named = tilo_action_policy::Grantee::agent(agent.to_owned());
            let answers: Vec<_> = policy
                .grants
                .iter()
                .filter(|grant| grant.grantee == named)
                .cloned()
                .collect();
            // The turn's answers are the agent's: whatever the workspace held for it before is
            // taken back first, as a person setting them anew replaces them.
            if let Some(held) = workspaces.policy_of(&workspace).await {
                let stale: Vec<_> = held
                    .grants
                    .iter()
                    .filter(|grant| grant.grantee == named)
                    .filter(|grant| !answers.iter().any(|answer| answer.id == grant.id))
                    .map(|grant| grant.id.clone())
                    .collect();
                if !stale.is_empty() {
                    crate::workspaces::fixtures::revoke_grants(workspaces, &workspace, &stale)
                        .await;
                }
            }
            if !answers.is_empty() {
                crate::workspaces::fixtures::record_grants(workspaces, &workspace, &answers).await;
            }
        }
    }
    let receipt = workspaces
        .device_actions()
        .invoke(caller, request.clone())
        .await
        .expect("the device's dispatch answers")?;
    match receipt.status {
        tilo_actions::remote::ReceiptStatus::Completed { output } => {
            Ok(output.value.unwrap_or_default())
        }
        other => panic!("a turn's call is answered at once or refused: {other:?}"),
    }
}

/// The rule of an agent's bound that answers `action` of `resource` of `provider` in
/// `workspace` alone with `answer`: an outside effect is allowed unasked only by a rule that
/// names its one action, which is the one form the workspace's list of its agents keeps.
pub(super) fn naming(
    workspace: &str,
    provider: &str,
    resource: &str,
    action: &str,
    answer: Answer,
) -> Rule {
    Rule::new(
        TreePath::workspace(workspace)
            .app(provider)
            .kind(resource)
            .rest()
            .action(action),
        KindSet::of(ActionKind::Outside),
        answer,
    )
}

/// A bound that narrows an agent in `workspace` to the changes of another app that can be
/// undone: none of what a person's vault, mail, calendars or connections answer.
pub(super) fn narrowed_elsewhere(workspace: &str) -> Vec<Rule> {
    vec![Rule::new(
        TreePath::workspace(workspace).app("so.tilo.notes"),
        KindSet::of(ActionKind::Undoable),
        Answer::Allow,
    )]
}

/// `agent` in `workspace`'s list of its agents, as `person` keeps it, with the rules `bound`
/// narrow it to as the workspace's grants on it.
pub(super) async fn listed(
    workspaces: &Workspaces,
    workspace: &str,
    person: &str,
    agent: &str,
    bound: &[Rule],
) {
    use tilo_resource::{Id, Replica};
    use tilo_workspace_agent::WorkspaceAgents;
    let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let kept = workspaces
        .resource_document(workspace, tilo_workspace_agent::OWNER, &record)
        .await
        .unwrap()
        .is_some_and(|bytes| {
            Replica::<WorkspaceAgents>::load(&bytes)
                .and_then(|record| record.state())
                .unwrap()
                .agents
                .find(&Id::from_raw(agent))
                .is_some()
        });
    if !kept {
        // The person makes the agent as anyone does, by the workspace's action, signed on a
        // device of theirs; the host's own person asks on the host's own device.
        let device = super::super::tests::admitted_device_of(person)
            .map_or_else(|| workspaces.device_cert().clone(), |(device, _)| device);
        let asking = super::super::tests::caller(&device);
        let mut creating = super::super::tests::request(
            workspace,
            tilo_actions::remote::WORKSPACE_AGENT_CREATE,
            serde_json::json!({"name": "Scout", "id": agent}),
        );
        creating = super::super::tests::signed_from(&creating, workspace, (&device, &asking));
        let workspace_id = collab_types::WorkspaceId::parse(workspace).unwrap();
        workspaces
            .agents_invoke(workspace_id, &asking, &creating)
            .await
            .expect("the person makes their agent");
    }
    let grants: Vec<_> = bound
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            tilo_action_policy::Grant::new(
                format!("test-narrowing:{index}"),
                tilo_action_policy::Grantee::agent(agent.to_owned()),
                rule.node.clone(),
                rule.kinds,
                rule.answer,
                tilo_action_policy::Grantee::person(person.to_owned()),
            )
        })
        .collect();
    if !grants.is_empty() {
        crate::workspaces::fixtures::record_grants(workspaces, workspace, &grants).await;
    }
}

/// The refusal the device's dispatch answers `who`, one of [`not_hosted_turns`], with: none
/// knows the call. An agent another of the person's devices hands its session to is answered as
/// any caller is: as unknown where this device offers no such action, and as refused where it
/// offers it and the agent's grants do not reach.
pub(super) fn not_a_turn<T: std::fmt::Debug + PartialEq>(
    who: &str,
    refused: Result<T, ActionCallError>,
) {
    if who == HANDED {
        assert!(
            matches!(refused, Err(UnknownAction | Refused { .. })),
            "{who}: {refused:?}"
        );
    } else {
        assert_eq!(refused, Err(UnknownAction), "{who}");
    }
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

/// What `taken` shows its person, where it waits for their yes.
pub(super) fn asks<T: std::fmt::Debug>(taken: Result<T, ActionCallError>) -> AskedFacts {
    match taken {
        Err(ActionCallError::ConfirmationRequired(facts)) => *facts,
        other => panic!("it waits for its person's yes: {other:?}"),
    }
}

pub(super) use super::super::tests::approved;

/// Whether `facts` name the agent `id`, known to its person as `name`, as who asks.
pub(super) fn asked_by(facts: &AskedFacts, id: &str, name: &str) -> bool {
    matches!(&facts.by, AskedBy::Agent(Named { id: by, name: called }) if by == id && called == name)
}

/// The fact `facts` show under `label`.
pub(super) fn fact<'a>(facts: &'a AskedFacts, label: &str) -> &'a str {
    facts
        .facts
        .iter()
        .find(|fact| fact.label == label)
        .map_or_else(
            || panic!("no {label} among {facts:?}"),
            |fact| fact.value.as_str(),
        )
}

/// The grant `ada`'s "always" gives `agent` for the use `target` makes of `item` at `places`,
/// written in `workspace` by its owner `owner`: what the workspace then lets stand.
pub(super) async fn always(
    workspaces: &Workspaces,
    (workspace, owner, ada): (&str, &DeviceCert, &DeviceCert),
    agent: &str,
    (target, item, places): (&RemoteActionTarget, &Value, &[&str]),
) {
    let id = item
        .as_str()
        .unwrap()
        .trim_start_matches("vault.item:")
        .to_owned();
    let mut places = places.to_vec();
    places.sort_unstable();
    let node = TreePath::workspace(workspace)
        .app(&target.provider)
        .kind(person_vault::use_word(target))
        .resource(format!("{id}/{}", json!(places)))
        .action(&target.address.action);
    let grant = Grant::new(
        format!("test:always:{agent}:{id}"),
        Grantee::agent(agent),
        node,
        KindSet::of(ActionKind::Outside),
        Answer::Allow,
        Grantee::person(hex::encode(ada.user_public_key)),
    );
    workspaces
        .put_workspace_grants(workspace, owner, rand::random(), vec![grant])
        .await
        .unwrap();
}

#[cfg(feature = "agents")]
/// The first use of an item by a turn the server carries out waits for its person and uses
/// nothing. What they are shown names the agent, its reason, the item by its kind and where it
/// goes, never the item's reference or its secret, and offers always; asked again, it waits
/// again. A decline never admits the call again, so nothing is used.
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
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
    };
    assert_eq!(workspaces.uses_offered(&turn).await.len(), 2);

    let adding = add_mail("ada@example.test", "imap.example.test", &login);
    let facts = asks(dispatched(workspaces, &turn, &adding).await);
    assert!(asked_by(&facts, "scout", "Scout"), "{facts:?}");
    assert_eq!(
        tilo_actions::remote::asked::asked_person(&scout),
        Some(hex::encode(ada.user_public_key)),
        "it asks the person whose turn it is"
    );
    assert_eq!(facts.words, "You asked me to read your mail.");
    assert_eq!(fact(&facts, "Item"), "login");
    assert_eq!(fact(&facts, "Goes to"), format!("{IMAP} {SMTP}"));
    assert_eq!(
        fact(&facts, "Account"),
        "ada@example.test",
        "where it signs in"
    );
    assert_eq!(facts.verb, MAIL_USE);
    assert_eq!(
        facts.plural.as_deref(),
        Some("vault items"),
        "always is offered"
    );
    // What the person is shown withholds the item.
    let shown = serde_json::to_string(&facts).unwrap();
    let id = login.as_str().unwrap().trim_start_matches("vault.item:");
    assert!(!shown.contains(id) && !shown.contains(PASSWORD), "{shown}");
    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");
    assert_eq!(
        remembered(&service, workspace_id, hers, &login).await,
        json!({"uses": []})
    );

    // Asked the same again, it waits again: the dispatch keeps one record per call.
    let again = add_mail("ada@example.test", "imap.example.test", &login);
    assert_eq!(asks(dispatched(workspaces, &turn, &again).await), facts);

    // Declined, the call is never admitted again: nothing is used and no grant stands.
    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");
    let remembers = remembered(&service, workspace_id, hers, &login).await;
    assert_eq!(remembers, json!({"uses": []}), "no use and no leave");
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}

/// Approved, a use is made once: it signs in with what the item keeps, answers the account
/// and never the password, is written down, and leaves no leave behind, so the next use asks
/// again. That an approve admits its call once is the asked store's (`asked_resolve`).
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
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
    };
    let add = |address: &str| add_mail(address, "imap.example.test", &login);

    let wanted = add("ada@example.test");
    asks(dispatched(workspaces, &turn, &wanted).await);
    let account = dispatched(workspaces, &turn, &approved(&wanted, &workspace))
        .await
        .expect("approved, it is made");
    assert_eq!(account["address"], "ada@example.test");
    assert!(!account.to_string().contains(PASSWORD), "{account}");
    assert_eq!(
        proved.lock().unwrap().clone(),
        vec![format!("ada@example.test with {PASSWORD}")]
    );
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
    asks(dispatched(workspaces, &turn, &wanted).await);
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
    let helper = Caller::delegated_agent("scout", ada.user_public_key, ada.device_public_key)
        .in_workspace(workspace_id.to_string());
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

/// With the grant "always" gives, the same agent uses the same item at the same servers
/// without asking, each use written down, while another agent of hers is still asked. The
/// person reads the leave and takes it back from their own device, which no agent does, and
/// then the next use asks again.
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
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
    };
    let add = |address: &str| add_mail(address, "imap.example.test", &login);

    // "Always" writes the leave, and the approved call is made.
    let wanted = add("news@example.test");
    asks(dispatched(workspaces, &turn, &wanted).await);
    always(
        workspaces,
        (&workspace, &owner, &ada),
        "scout",
        (&wanted.target, &login, &[IMAP, SMTP]),
    )
    .await;
    dispatched(workspaces, &turn, &approved(&wanted, &workspace))
        .await
        .expect("allowed always, it is made");
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
    let second = dispatched(workspaces, &turn, &add("work@example.test")).await;
    assert!(
        matches!(&second, Ok(account) if account["address"] == "work@example.test"),
        "{second:?}"
    );
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

    // The leave names that one use of that one item: another item of hers is still put to her,
    // never refused for what the leave does not name.
    let another = a_mail_login(&service, workspace_id, hers).await;
    let wanted = add_mail("other@example.test", IMAP, &another);
    asks(dispatched(workspaces, &turn, &wanted).await);
    assert_eq!(proved.lock().unwrap().len(), 2, "nothing more signed in");

    // Another agent of hers is still asked: the leave names one agent.
    let other = on_turn(&ada, &workspace, "other");
    let others = HostedTurn {
        caller: &other,
        ..turn
    };
    let wanted = add("more@example.test");
    asks(dispatched(workspaces, &others, &wanted).await);

    // She takes the leave back, and then her agent is asked again.
    let taken = she_alone_takes_the_leave_back(&service, workspace_id, &ada, &login).await;
    assert_eq!(taken.get("always"), None, "{taken}");
    assert_eq!(taken["uses"].as_array().unwrap().len(), 2);
    let wanted = add("late@example.test");
    asks(dispatched(workspaces, &turn, &wanted).await);
    assert_eq!(proved.lock().unwrap().len(), 2, "nothing more signed in");
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
        (HANDED, handed),
    ]
}

/// What [`not_hosted_turns`] calls the agent a person's own device hands its session to.
pub(super) const HANDED: &str = "an agent her own device hands its session to";

#[cfg(feature = "agents")]
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
            policy: None,
        };
        not_a_turn(who, dispatched(workspaces, &turn, &adding).await);
        assert!(workspaces.uses_offered(&turn).await.is_empty(), "{who}");
    }

    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");

    // The same agent, asked by Ada and by Bo, is offered the same uses and reaches the same
    // item: each ask is put to whoever asked.
    let ada_id = hex::encode(ada.user_public_key);
    let bo_id = hex::encode(bo.user_public_key);
    let for_ada = on_turn(&ada, &workspace, "scout");
    let for_bo = on_turn(&bo, &workspace, "scout");
    let (hers, his) = (
        HostedTurn {
            workspace: workspace_id,
            caller: &for_ada,
            policy: None,
        },
        HostedTurn {
            workspace: workspace_id,
            caller: &for_bo,
            policy: None,
        },
    );
    assert_eq!(
        workspaces.uses_offered(&hers).await,
        workspaces.uses_offered(&his).await
    );
    let facts = asks(dispatched(workspaces, &his, &adding).await);
    assert_eq!(
        tilo_actions::remote::asked::asked_person(his.caller),
        Some(bo_id),
        "the workspace's item asks whoever asked"
    );
    assert!(asked_by(&facts, "scout", "Scout"), "{facts:?}");
    let facts = asks(dispatched(workspaces, &hers, &adding).await);
    assert_eq!(
        tilo_actions::remote::asked::asked_person(hers.caller),
        Some(ada_id)
    );
    assert!(asked_by(&facts, "scout", "Scout"), "{facts:?}");

    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");
    assert_eq!(
        remembered(&service, workspace_id, (&ada, &as_ada), &login).await,
        json!({"uses": []})
    );
    server.shutdown().await;
}

#[cfg(feature = "agents")]
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
    let scout = on_turn(&ada, &workspace, "scout");
    let other = on_turn(&ada, &workspace, "other");
    let turn = |caller| HostedTurn {
        workspace: workspace_id,
        caller,
        policy: None,
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
        dispatched(workspaces, &turn(&scout), &adding).await,
        Err(NotPermitted),
        "the agent is kept from the vault"
    );
    assert!(workspaces.uses_offered(&turn(&scout)).await.is_empty());
    assert!(
        remembered(&service, workspace_id, (&ada, &as_ada), &login).await["uses"].is_array(),
        "Ada still reads the item herself"
    );
    let facts = asks(dispatched(workspaces, &turn(&other), &adding).await);
    assert!(
        asked_by(&facts, "other", "Scout"),
        "another agent is not held by the grant: {facts:?}"
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
    let facts = asks(dispatched(workspaces, &turn(&scout), &adding).await);
    assert!(
        asked_by(&facts, "scout", "Scout"),
        "without the grant the agent reaches the item: {facts:?}"
    );
    server.shutdown().await;
}

#[cfg(feature = "agents")]
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
    let adding = add_mail("ada@example.test", "imap.example.test", &login);
    let named = |action: &str, answer: Answer| {
        naming(
            &workspace,
            person_accounts::PROVIDER,
            person_accounts::RESOURCE,
            action,
            answer,
        )
    };
    // Every change of the provider's that can be undone: no outside effect.
    let whole_provider = Rule::new(
        TreePath::workspace(&workspace).app(person_accounts::PROVIDER),
        KindSet::of(ActionKind::Undoable),
        Answer::Allow,
    );

    for (why, bound) in [
        (
            "a rule for another app alone",
            narrowed_elsewhere(&workspace),
        ),
        (
            "a rule for the whole provider's changes",
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
        let policy = narrowing(&workspace_id.to_string(), &scout, &bound);
        let turn = HostedTurn {
            workspace: workspace_id,
            caller: &scout,
            policy: Some(&policy),
        };
        let refused = dispatched(workspaces, &turn, &adding).await;
        assert_eq!(refused, Err(NotPermitted), "{why}");
        assert!(
            workspaces.uses_offered(&turn).await.is_empty(),
            "{why}: it is not offered what it would be refused"
        );
    }

    let bound = vec![named(person_accounts::ADD_MAIL_WITH_ITEM, Answer::Allow)];
    let policy = narrowing(&workspace_id.to_string(), &scout, &bound);
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: Some(&policy),
    };
    let offered = workspaces.uses_offered(&turn).await;
    assert_eq!(offered.len(), 1, "the one use its bound names");
    asks(dispatched(workspaces, &turn, &adding).await);
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
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
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
        let refused = dispatched(workspaces, &turn, &asking).await;
        assert!(
            matches!(&refused, Err(Refused { reason }) if reason == words),
            "{words}: {refused:?}"
        );
    }
    let as_key = request(
        model_providers::target(model_providers::SET_KEY_FROM_ITEM),
        json!({"provider": provider, "key": login}),
    );
    let wrong_kind = dispatched(workspaces, &turn, &as_key).await;
    assert!(
        matches!(&wrong_kind, Err(Refused { .. })),
        "a login is no key: {wrong_kind:?}"
    );
    // A password typed where the reference belongs is refused unread.
    let typed = add_mail("ada@example.test", "imap.example.test", &json!(PASSWORD));
    let unread = dispatched(workspaces, &turn, &typed).await;
    assert!(
        matches!(&unread, Err(InvalidRequest { reason }) if !reason.contains(PASSWORD)),
        "{unread:?}"
    );
    let other = request(person_accounts::target(person_accounts::LIST), json!({}));
    assert_eq!(
        dispatched(workspaces, &turn, &other).await,
        Err(UnknownAction),
        "only the uses by reference are taken here"
    );
    assert!(proved.lock().unwrap().is_empty(), "nothing signed in");
    server.shutdown().await;
}

/// A provider's key goes the same way as a mail sign-in: asked for first, with the question
/// saying which key and where, and with the grant "always" gives kept without a question, each
/// use written down.
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
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        policy: None,
    };
    let keeping = || {
        request(
            model_providers::target(model_providers::SET_KEY_FROM_ITEM),
            json!({"provider": provider, "key": api_key}),
        )
    };

    let first = keeping();
    let facts = asks(dispatched(workspaces, &turn, &first).await);
    assert_eq!(fact(&facts, "Item"), "key");
    assert_eq!(fact(&facts, "Goes to"), "https://models.example");
    always(
        workspaces,
        (&workspace, &owner, &ada),
        "scout",
        (&first.target, &api_key, &["https://models.example"]),
    )
    .await;
    let view = dispatched(workspaces, &turn, &approved(&first, &workspace))
        .await
        .expect("allowed always, the key is kept");
    assert_eq!(view["configured"], json!(true), "{view}");
    let alone = dispatched(workspaces, &turn, &keeping()).await;
    assert!(alone.is_ok(), "{alone:?}");
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

/// A workspace policy whose answers on the agent `caller` acts as are `rules`, set by the
/// person who gave them: each kind of a rule is its own answer, as setting answers anew does.
pub(super) fn narrowing(
    workspace: &str,
    caller: &Caller,
    rules: &[Rule],
) -> tilo_action_policy::WorkspacePolicy {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let mut policy = tilo_action_policy::WorkspacePolicy::new(workspace, "owner");
    let agent = caller.acting().agent_id().unwrap_or_default();
    for rule in rules {
        let scope = AnswerScope::of_node(&rule.node)
            .expect("a rule of a bound stands at a node an answer names");
        for kind in rule.kinds.iter() {
            policy.grants.push(agent_answers::grant(
                workspace,
                &agent,
                (&scope, kind),
                rule.answer,
                tilo_action_policy::Grantee::person("owner"),
            ));
        }
    }
    policy
}
