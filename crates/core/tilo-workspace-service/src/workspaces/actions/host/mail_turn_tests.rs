//! A workspace's mail and a turn this server carries out for a member: the
//! turn reads, searches and drafts with no device behind it wherever the
//! workspace's policy lets that member reach its mail; every other turn
//! reaches nothing and is offered nothing; and a message a turn would send is
//! set apart until its person approves it, leaves as they were shown it, once,
//! and is found again after the server stopped.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, and the server handle a test binds is kept until the test ends"
)]

use std::sync::Arc;

use serde_json::{Value, json};
use tilo_action_policy::{ActionKind, Answer, KindSet, Rule, TreePath};
use tilo_actions::remote::asked::{AskedFacts, asked_person};
use tilo_actions::remote::person_accounts::{
    self, FetchedMail, FetchedMessage, FoundMail, MailAccounts, MailArrivals, StartedSend,
};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions as HolderActions;
use super::super::tests::{admit, governed};
use super::accounts_tests::asking;
use super::mail_assistant_tests::{lunch, lunch_with};
use super::turn_use_tests::{
    approved, asks, dispatched, naming, narrowed_elsewhere, not_a_turn, not_hosted_turns, on_turn,
};
use crate::workspaces::hosted_use::HostedTurn;
use crate::workspaces::mail_watch::testing::{Watched, keep};
use crate::workspaces::tests::{governor, test_host as test_server};
use crate::workspaces::{RemoveMemberRequest, WorkspaceId, Workspaces};

const ACCOUNT: &str = "ada@example.test";
const PASSWORD: &str = "adas-password";
const SCOUT: &str = "scout";
/// What the lunch to Bo alone is handed to the servers as.
const LUNCH_LEFT: &str = "send ada@example.test to bo@example.test: Lunch";

/// Ada in a workspace of a server whose mail servers answer and keep what
/// they were asked: a device the workspace admits her on, and one account the
/// workspace keeps signed in.
struct Mail {
    servers: Arc<Watched>,
    workspaces: Workspaces,
    workspace: String,
    workspace_id: WorkspaceId,
    /// The workspace's actions, held while the turns ask of its mail.
    _service: HolderActions,
    ada: DeviceCert,
    owner: DeviceCert,
}

impl Mail {
    async fn on(workspaces: &Workspaces, owner: &DeviceCert) -> Self {
        let servers = Arc::new(Watched::answering());
        workspaces.carry_mail_through(servers.clone());
        let (workspace, workspace_id, service) = governed(workspaces).await;
        let ada = admit(workspaces, &workspace, owner, 1).await;
        keep(workspaces, &workspace_id.to_string(), ACCOUNT, PASSWORD).await;
        Self {
            servers,
            workspaces: workspaces.clone(),
            workspace,
            workspace_id,
            _service: service,
            ada,
            owner: owner.clone(),
        }
    }

    /// Ada, by her person id: who asks the turns of these tests.
    fn ada_id(&self) -> String {
        hex::encode(self.ada.user_public_key)
    }

    /// The turn `caller` acts on in her workspace, for an agent `bound`
    /// narrows.
    const fn turn<'a>(&self, caller: &'a Caller, bound: Option<&'a [Rule]>) -> HostedTurn<'a> {
        HostedTurn {
            workspace: self.workspace_id,
            caller,
            bound,
        }
    }

    /// What `turn` is answered when it asks `action` of the workspace's mail.
    async fn asks(
        &self,
        turn: &HostedTurn<'_>,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionCallError> {
        dispatched(&self.workspaces, turn, &asking(action, argument)).await
    }

    /// The mail actions `turn` is offered, by their ids.
    async fn offered(&self, turn: &HostedTurn<'_>) -> Vec<String> {
        let rows = self.workspaces.mail_offered(turn).await;
        rows.into_iter()
            .filter_map(|row| row.action.map(|action| action.address.action))
            .collect()
    }

    /// `turn` begins `begun` and asks for it to be sent, which waits for its person: the
    /// call, and what they are shown.
    async fn sends(&self, turn: &HostedTurn<'_>, begun: Value) -> (InvokeRequest, AskedFacts) {
        let started: StartedSend = done(self.asks(turn, person_accounts::BEGIN_SEND, begun).await);
        let sending = asking(person_accounts::SEND, json!({"send": started.send}));
        let shown = asks(dispatched(&self.workspaces, turn, &sending).await);
        (sending, shown)
    }

    /// `request` of `turn` again, as its person's approve admits it.
    async fn approve(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        approve_in(&self.workspaces, turn, request).await
    }

    /// Remove Ada from the workspace as its owner does, so its policy no
    /// longer lets her reach its mail.
    async fn remove_ada(&self) {
        self.workspaces
            .remove_member(
                &self.workspace,
                &self.owner,
                RemoveMemberRequest {
                    request_id: rand::random(),
                    user_public_key: self.ada.user_public_key,
                },
            )
            .await
            .expect("the owner removes her");
    }

    /// What left through the mail servers, oldest first.
    fn left(&self) -> Vec<String> {
        let asked = self.servers.asked();
        asked
            .into_iter()
            .filter(|asked| asked.starts_with("send"))
            .collect()
    }
}

/// `request` of `turn` in `workspaces` again, as its person's approve admits it.
async fn approve_in(
    workspaces: &Workspaces,
    turn: &HostedTurn<'_>,
    request: &InvokeRequest,
) -> Result<Value, ActionCallError> {
    dispatched(
        workspaces,
        turn,
        &approved(request, &turn.workspace.to_string()),
    )
    .await
}

/// What an answered request carries, read as `T`.
fn done<T: serde::de::DeserializeOwned>(answer: Result<Value, ActionCallError>) -> T {
    match answer {
        Ok(value) => serde_json::from_value(value).expect("the answer reads"),
        other => panic!("the turn is answered: {other:?}"),
    }
}

/// What its person is shown of a send that waits for them, a row to a line.
fn words(shown: &AskedFacts) -> String {
    shown
        .facts
        .iter()
        .map(|fact| format!("{}: {}", fact.label, fact.value))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One message of the inbox, as a request to read it names it.
fn reading() -> Value {
    json!({"account": ACCOUNT, "folder": "inbox", "uid": 4, "uidValidity": 7})
}

/// The workspace keeps the account, and the turn of a member its policy lets
/// read its mail is offered the mail actions, lists the workspace's account,
/// reads what arrived, a folder and a message, searches, and keeps a draft,
/// and nothing leaves. What only a person does is no action of a turn, and a
/// member the workspace removes reaches nothing again.
#[tokio::test]
async fn a_members_turn_reads_searches_and_drafts() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_server(dir.path(), &key, &owner).await;
    let mail = Mail::on(server.workspaces(), &owner).await;
    let scout = on_turn(&mail.ada, &mail.workspace, SCOUT);
    let turn = mail.turn(&scout, None);
    let nobody = Err(ActionCallError::UnknownAction);

    assert_eq!(
        mail.offered(&turn).await,
        [
            person_accounts::LIST,
            person_accounts::ARRIVED,
            person_accounts::FETCH,
            person_accounts::READ,
            person_accounts::SEARCH,
            person_accounts::BEGIN_SEND,
            person_accounts::SEND_PIECE,
            person_accounts::SAVE_DRAFT,
            person_accounts::SEND,
        ]
    );
    let listed: MailAccounts = done(mail.asks(&turn, person_accounts::LIST, json!({})).await);
    assert_eq!(listed.accounts.len(), 1);
    assert_eq!(listed.accounts[0].id, ACCOUNT);
    let _: MailArrivals = done(mail.asks(&turn, person_accounts::ARRIVED, json!({})).await);
    let folder = json!({"account": ACCOUNT, "folder": "inbox"});
    let fetched: FetchedMail = done(mail.asks(&turn, person_accounts::FETCH, folder).await);
    assert_eq!(fetched.messages.len(), 1);
    let message: FetchedMessage = done(mail.asks(&turn, person_accounts::READ, reading()).await);
    assert_eq!(message.body, "Friday at one?");
    let search = json!({"account": ACCOUNT, "text": "menu"});
    let found: FoundMail = done(mail.asks(&turn, person_accounts::SEARCH, search).await);
    assert_eq!(found.folders.len(), 1);
    let begun: StartedSend = done(mail.asks(&turn, person_accounts::BEGIN_SEND, lunch()).await);
    let keeping = json!({"send": begun.send});
    let kept = mail.asks(&turn, person_accounts::SAVE_DRAFT, keeping).await;
    assert!(matches!(kept, Ok(_)), "{kept:?}");
    let asked = mail.servers.asked();
    for what in [
        "fetch ada@example.test inbox",
        "read ada@example.test inbox 4",
        "search ada@example.test menu",
        "draft ada@example.test to bo@example.test: Lunch",
    ] {
        assert!(asked.iter().any(|asked| asked == what), "{what}: {asked:?}");
    }
    assert!(mail.left().is_empty(), "a draft leaves nothing");

    // What only a person does is no action a turn takes.
    for (action, argument) in [
        (person_accounts::REMOVE, json!({"account": ACCOUNT})),
        (person_accounts::MARK, reading()),
        (person_accounts::ADD_MAIL, json!({"address": ACCOUNT})),
        (person_accounts::SET_SERVICES, json!({"account": ACCOUNT})),
    ] {
        assert_eq!(mail.asks(&turn, action, argument).await, nobody, "{action}");
    }

    // Removed from the workspace, she is let reach nothing of its mail.
    mail.remove_ada().await;
    assert!(
        mail.offered(&turn).await.is_empty(),
        "the workspace removed her"
    );
    let after = mail.asks(&turn, person_accounts::READ, reading()).await;
    assert_eq!(after, nobody);
    server.shutdown().await;
}

/// Every caller that is not a turn of a member the workspace lets read its
/// mail reaches nothing of it and is offered nothing: a person, an agent
/// outside a turn, an agent admitted as a member, a bot, an app, a turn
/// confined nowhere or elsewhere, a turn on a device the workspace does not
/// admit that person on, and a turn kept by somebody else's person. Another
/// member's turn in the same workspace reads it, as the one store it is. No
/// mail server is asked for the refused.
#[tokio::test]
async fn only_a_members_turn_reaches_the_workspaces_mail() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_server(dir.path(), &key, &owner).await;
    let mail = Mail::on(server.workspaces(), &owner).await;
    let bo = admit(&mail.workspaces, &mail.workspace, &owner, 2).await;
    let (elsewhere, elsewhere_id, _) = governed(&mail.workspaces).await;
    let cy = admit(&mail.workspaces, &elsewhere, &owner, 3).await;
    let scout = on_turn(&mail.ada, &mail.workspace, SCOUT);
    let hers = mail.turn(&scout, None);
    let nobody = Err(ActionCallError::UnknownAction);

    let mut others = not_hosted_turns(&mail.ada, &mail.workspace);
    others.push((
        "a turn in another workspace",
        Caller::delegated_agent(SCOUT, cy.user_public_key, cy.device_public_key)
            .in_workspace(&elsewhere)
            .on_roster(0, cy.user_public_key)
            .on_roster_turn(tilo_action_policy::turn::Turn::new(
                "chat/main",
                "message-1",
            )),
    ));
    for (who, asks) in others {
        let turn = mail.turn(&asks, None);
        not_a_turn(
            who,
            mail.asks(&turn, person_accounts::READ, reading()).await,
        );
        assert!(mail.offered(&turn).await.is_empty(), "{who}");
    }
    // A turn carried out in the other workspace reaches that one's mail,
    // which keeps none.
    let cys = on_turn(&cy, &elsewhere, SCOUT);
    let there = HostedTurn {
        workspace: elsewhere_id,
        caller: &cys,
        bound: None,
    };
    let reads = mail.asks(&there, person_accounts::READ, reading()).await;
    assert_eq!(reads, nobody, "a workspace that keeps no mail");
    assert!(mail.offered(&there).await.is_empty());

    assert!(mail.servers.asked().is_empty(), "no mail server was asked");

    // Another member's turn in the same workspace reads the one store.
    let bos = on_turn(&bo, &mail.workspace, SCOUT);
    let his = mail.turn(&bos, None);
    let message: FetchedMessage = done(mail.asks(&his, person_accounts::READ, reading()).await);
    assert_eq!(message.uid, 4, "a member's turn reads the workspace's mail");
    let hers_read: FetchedMessage = done(mail.asks(&hers, person_accounts::READ, reading()).await);
    assert_eq!(hers_read.uid, 4, "and so does hers");
    server.shutdown().await;
}

/// What a turn reaches of the workspace's mail is decided at Mail, for the member who asked:
/// an owner's grant keeping Bo out of Mail keeps every turn he asks out of its accounts,
/// though the action is the accounts provider's, and he is offered nothing of it. Ada, whom no
/// grant keeps out, asks the same agent and reads. No mail server is asked for Bo.
#[tokio::test]
async fn a_member_kept_out_of_mail_keeps_the_turns_he_asks_out_of_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_server(dir.path(), &key, &owner).await;
    let mail = Mail::on(server.workspaces(), &owner).await;
    let bo = admit(&mail.workspaces, &mail.workspace, &owner, 2).await;
    let kept_out = tilo_action_policy::Grant::new(
        "test:bo-kept-out-of-mail",
        tilo_action_policy::Grantee::person(hex::encode(bo.user_public_key)),
        TreePath::workspace(&mail.workspace).app("so.tilo.mail"),
        KindSet::ALL,
        Answer::Deny,
        tilo_action_policy::Grantee::person(hex::encode(owner.user_public_key)),
    );
    mail.workspaces
        .put_workspace_grants(&mail.workspace, &owner, [63; 32], vec![kept_out])
        .await
        .unwrap();

    let bos = on_turn(&bo, &mail.workspace, SCOUT);
    let his = mail.turn(&bos, None);
    let reads = mail.asks(&his, person_accounts::READ, reading()).await;
    assert_eq!(
        reads,
        Err(ActionCallError::UnknownAction),
        "Bo's turn reads nothing"
    );
    assert!(
        mail.offered(&his).await.is_empty(),
        "and is offered nothing"
    );
    assert!(mail.servers.asked().is_empty(), "no mail server was asked");

    let adas = on_turn(&mail.ada, &mail.workspace, SCOUT);
    let hers = mail.turn(&adas, None);
    let message: FetchedMessage = done(mail.asks(&hers, person_accounts::READ, reading()).await);
    assert_eq!(message.uid, 4, "the same agent asked by Ada reads");
    server.shutdown().await;
}

/// An agent its entry narrows reaches a mail action only where a rule of its
/// bound names exactly that action: no rule, a rule for the whole provider,
/// a rule for another action and a rule that denies it all refuse, and it is
/// offered only what a rule names. With the rule for a send it still asks
/// its person first.
#[tokio::test]
async fn a_narrowed_agent_needs_a_rule_that_names_the_mail_action() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_server(dir.path(), &key, &owner).await;
    let mail = Mail::on(server.workspaces(), &owner).await;
    let sub = on_turn(&mail.ada, &mail.workspace, "sub-agent");
    let named = |action: &str, answer: Answer| {
        naming(
            &mail.workspace,
            person_accounts::PROVIDER,
            person_accounts::RESOURCE,
            action,
            answer,
        )
    };
    // Every change of the provider's that can be undone: no outside effect.
    let whole_provider = Rule::new(
        TreePath::workspace(&mail.workspace).app(person_accounts::PROVIDER),
        KindSet::of(ActionKind::Undoable),
        Answer::Allow,
    );
    let listing = named(person_accounts::LIST, Answer::Allow);
    let denying = named(person_accounts::READ, Answer::Deny);
    for (why, bound) in [
        (
            "a rule for another app alone",
            narrowed_elsewhere(&mail.workspace),
        ),
        (
            "a rule for the whole provider's changes",
            vec![whole_provider.clone()],
        ),
        ("a rule for another action", vec![listing]),
        ("a rule that denies it", vec![whole_provider, denying]),
    ] {
        let turn = mail.turn(&sub, Some(&bound));
        let reads = mail.asks(&turn, person_accounts::READ, reading()).await;
        assert_eq!(reads, Err(ActionCallError::NotPermitted), "{why}");
        let offered = mail.offered(&turn).await;
        assert!(
            !offered.iter().any(|action| action == person_accounts::READ),
            "{why}: it is not offered what it would be refused: {offered:?}"
        );
    }
    assert!(mail.servers.asked().is_empty(), "no mail server was asked");

    let bound = vec![
        named(person_accounts::READ, Answer::Allow),
        named(person_accounts::BEGIN_SEND, Answer::Allow),
    ];
    let turn = mail.turn(&sub, Some(&bound));
    assert_eq!(
        mail.offered(&turn).await,
        [person_accounts::READ, person_accounts::BEGIN_SEND]
    );
    let message: FetchedMessage = done(mail.asks(&turn, person_accounts::READ, reading()).await);
    assert_eq!(message.uid, 4);
    let begun: StartedSend = done(mail.asks(&turn, person_accounts::BEGIN_SEND, lunch()).await);
    let sending = json!({"send": begun.send});
    let sends = mail
        .asks(&turn, person_accounts::SEND, sending.clone())
        .await;
    assert_eq!(
        sends,
        Err(ActionCallError::NotPermitted),
        "no rule names a send"
    );

    let mut bound = bound;
    bound.push(named(person_accounts::SEND, Answer::Allow));
    let turn = mail.turn(&sub, Some(&bound));
    asks(mail.asks(&turn, person_accounts::SEND, sending).await);
    assert!(
        mail.left().is_empty(),
        "a rule of its bound is no answer of its person's: it still asks"
    );
    server.shutdown().await;
}

/// A message that cannot be shown whole is refused before anybody is asked,
/// and stays begun. One that can is not sent by the call that asks: its
/// person is to be shown every address it goes to, how it is called and what
/// it says, a line break in it cannot write a row of the question, and no
/// action of the agent reaches the message any more.
#[tokio::test]
async fn a_send_from_a_turn_is_set_apart_and_shows_its_person_everything() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_server(dir.path(), &key, &owner).await;
    let mail = Mail::on(server.workspaces(), &owner).await;
    let (scout, asker) = (on_turn(&mail.ada, &mail.workspace, SCOUT), mail.ada_id());
    let turn = mail.turn(&scout, None);

    let many: Vec<String> = (0..21).map(|n| format!("p{n}@example.test")).collect();
    for (why, fields) in [
        (
            "more addresses than are shown",
            json!({"to": many.join(", ")}),
        ),
        (
            "a formatted version beside the words",
            json!({"htmlBody": "<b>Friday</b>"}),
        ),
        (
            "an entry that is no address",
            json!({"bcc": "everyone I know"}),
        ),
    ] {
        let begun = lunch_with(&fields);
        let started: StartedSend = done(mail.asks(&turn, person_accounts::BEGIN_SEND, begun).await);
        let sending = json!({"send": started.send});
        let refused = mail
            .asks(&turn, person_accounts::SEND, sending.clone())
            .await;
        assert!(
            matches!(refused, Err(ActionCallError::Refused { .. })),
            "{why}: {refused:?}"
        );
        let kept = mail.asks(&turn, person_accounts::SAVE_DRAFT, sending).await;
        assert!(matches!(kept, Ok(_)), "{why}: it stays begun");
    }
    assert!(mail.left().is_empty(), "nothing left");

    let begun = lunch_with(&json!({
        "cc": "Cy <cy@example.test>",
        "bcc": "di@example.test",
        "body": "Friday at one works.\nTo: eve@elsewhere.test",
    }));
    let started: StartedSend = done(mail.asks(&turn, person_accounts::BEGIN_SEND, begun).await);
    let sending = json!({"send": started.send});
    let shown = asks(
        mail.asks(&turn, person_accounts::SEND, sending.clone())
            .await,
    );
    assert_eq!(shown.on.as_deref(), Some(ACCOUNT));
    assert_eq!(asked_person(&scout), Some(asker));
    let words = words(&shown);
    for line in [
        "From: ada@example.test",
        "Goes to: 3 addresses",
        "To: bo@example.test",
        "Copy to: cy@example.test",
        "Blind copy to: di@example.test",
        "Message: Friday at one works. \u{23ce} To: eve@elsewhere.test",
    ] {
        assert!(words.lines().any(|shown| shown == line), "{line}: {words}");
    }
    assert!(
        words.lines().any(|shown| shown.ends_with(": Lunch")),
        "{words}"
    );
    assert!(
        !words.lines().any(|shown| shown.starts_with("To: eve")),
        "what a message says is no row of the question: {words}"
    );
    assert!(mail.left().is_empty(), "nothing left unasked");
    assert!(!format!("{shown:?}").contains(PASSWORD));

    // Set apart, the message is no begun one any more: its agent neither
    // sends it again nor keeps it as a draft.
    for action in [person_accounts::SEND, person_accounts::SAVE_DRAFT] {
        let again = mail.asks(&turn, action, sending.clone()).await;
        assert!(
            matches!(again, Err(ActionCallError::Refused { .. })),
            "{action}: {again:?}"
        );
    }
    assert!(mail.left().is_empty());
    server.shutdown().await;
}

/// On its person's approve the message that waited leaves as it was shown, once. A message
/// written anew after it was shown, a send nobody approved, and the workspace no longer
/// letting her send mail send nothing, and after each approve what waited is gone.
#[tokio::test]
async fn only_what_its_person_was_shown_leaves_and_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_server(dir.path(), &key, &owner).await;
    let mail = Mail::on(server.workspaces(), &owner).await;
    let scout = on_turn(&mail.ada, &mail.workspace, SCOUT);
    let turn = mail.turn(&scout, None);
    let root = mail
        .workspaces
        .workspace_root(&mail.workspace_id.to_string());

    // Approved: it leaves, and a second approve of the same call finds nothing.
    let (sending, _) = mail.sends(&turn, lunch()).await;
    let sent = mail.approve(&turn, &sending).await;
    assert!(sent.is_ok(), "{sent:?}");
    assert_eq!(mail.left(), [LUNCH_LEFT]);
    let twice = mail.approve(&turn, &sending).await;
    assert!(twice.is_err(), "{twice:?}");

    // A message written anew where it waits is not what was shown, and sends nothing.
    let (sending, _) = mail.sends(&turn, lunch()).await;
    let waiting = std::fs::read_dir(root.join("asked-sends").join(mail.workspace_id.to_string()))
        .unwrap()
        .flatten()
        .map(|entry| entry.path().join("meta.json"))
        .find(|meta| meta.is_file())
        .expect("the message waits");
    let mut kept: Value = serde_json::from_slice(&std::fs::read(&waiting).unwrap()).unwrap();
    kept["message"]["to"] = json!("eve@elsewhere.test");
    std::fs::write(&waiting, serde_json::to_vec(&kept).unwrap()).unwrap();
    let refused = mail.approve(&turn, &sending).await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("not what you were shown")),
        "{refused:?}"
    );
    let gone = mail.approve(&turn, &sending).await;
    assert!(
        gone.is_err(),
        "what waited is gone with the approve: {gone:?}"
    );

    // Nobody approved it: it waits, and nothing leaves.
    mail.sends(&turn, lunch()).await;

    // The workspace stopped letting her send while the question waited.
    let (sending, _) = mail.sends(&turn, lunch()).await;
    mail.remove_ada().await;
    let refused = mail.approve(&turn, &sending).await;
    assert!(refused.is_err(), "{refused:?}");

    assert_eq!(mail.left(), [LUNCH_LEFT], "one message left, once");
    server.shutdown().await;
}

/// A message set apart is found again after the server stopped: on its person's approve it
/// leaves once, and a second approve sends nothing.
#[tokio::test]
async fn a_message_that_waits_is_found_again_after_the_server_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    // The device's keys stay its own across the restart, as its key service keeps them.
    let keys = Arc::<crate::workspaces::fixtures::TestGroupKeys>::default();
    let server =
        crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &owner, keys.clone())
            .await;
    let mail = Mail::on(server.workspaces(), &owner).await;
    let scout = on_turn(&mail.ada, &mail.workspace, SCOUT);
    let (sending, _) = mail.sends(&mail.turn(&scout, None), lunch()).await;
    let workspace_id = mail.workspace_id;
    assert!(mail.left().is_empty());
    drop(mail);

    server.shutdown().await;
    let server =
        crate::workspaces::fixtures::test_host_holding(dir.path(), &key, &owner, keys).await;
    let workspaces = server.workspaces();
    let servers = Arc::new(Watched::answering());
    workspaces.carry_mail_through(servers.clone());
    let turn = HostedTurn {
        workspace: workspace_id,
        caller: &scout,
        bound: None,
    };
    let sent = approve_in(workspaces, &turn, &sending).await;
    assert!(sent.is_ok(), "{sent:?}");
    assert_eq!(servers.asked(), [LUNCH_LEFT]);

    let twice = approve_in(workspaces, &turn, &sending).await;
    assert!(twice.is_err(), "{twice:?}");
    assert_eq!(servers.asked(), [LUNCH_LEFT], "nothing leaves twice");
    server.shutdown().await;
}
