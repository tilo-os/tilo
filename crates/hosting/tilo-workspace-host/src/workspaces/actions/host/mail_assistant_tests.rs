//! A person's mail and the agents of theirs: only the person says which
//! agent may read which account; the one they named reads, searches and
//! drafts through the host; a message it would send waits for its person;
//! and nobody else's agent is anybody to that mail.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host a test binds is kept until the test ends; dropping it earlier closes what it drives"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_action_policy::turn::Turn;
use tilo_actions::remote::person_accounts::{
    self, AccountAssistants, FetchedMail, FetchedMessage, FoundMail, MailAccounts, MailArrivals,
    StartedSend,
};
use tilo_actions::remote::{ActionHost, ActionHostError, Caller, CatalogueQuery, ReceiptStatus};
use tilo_relay_protocol::WorkspaceId;
use tilo_relay_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_user_identity::DeviceCert;

use super::super::HostWorkspaceActions;
use super::super::tests::{caller, settled};
use super::accounts_tests::{asking, key_of};
use crate::workspaces::fixtures::TestHost;
use crate::workspaces::mail_watch::testing::{Watched, keep};
use crate::workspaces::tests::{governor, test_host};
use crate::workspaces::{AsksPeople, PersonAnswer, Workspaces};

const ACCOUNT: &str = "ada@example.test";
const ASSISTANT: &str = "assistant";

/// Ada's desk: a host whose mail servers answer and keep what they were
/// asked, a device of hers, the workspace its page is in, and one account
/// she keeps signed in.
struct Desk {
    servers: Arc<Watched>,
    workspaces: Workspaces,
    host: HostWorkspaceActions,
    device: DeviceCert,
    workspace_id: WorkspaceId,
    _relay: TestHost,
    _dir: tempfile::TempDir,
}

impl Desk {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces().clone();
        let servers = Arc::new(Watched::answering());
        workspaces.carry_mail_through(servers.clone());
        let host = HostWorkspaceActions::new();
        host.attach(workspaces.clone());
        let device = key_of(&SigningKey::from_bytes(&rand::random()), "studio");
        keep(
            &workspaces.person_settings_root(&device.user_public_key),
            ACCOUNT,
            "adas-password",
        );
        let workspace_id = workspaces.create("Mail").await.unwrap().workspace_id;
        Self {
            servers,
            workspaces,
            host,
            device,
            workspace_id,
            _relay: relay,
            _dir: dir,
        }
    }

    /// Ada's page: what a device of hers asks the host through.
    fn page(&self) -> impl ActionHost + Clone + Send + 'static {
        self.host.in_workspace_as(self.workspace_id, &self.device)
    }

    /// Ada, on her device.
    fn ada(&self) -> Caller {
        caller(&self.device)
    }

    /// The agent `name` on Ada's device, over the connection that device
    /// certified.
    fn agent(&self, name: &str) -> Caller {
        Caller::delegated_agent(
            name,
            self.device.user_public_key,
            self.device.device_public_key,
        )
    }

    /// What `caller` is answered when it asks `action` over Ada's page.
    async fn ask(
        &self,
        caller: &Caller,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionHostError> {
        asked_of(&self.page(), caller, action, argument).await
    }

    /// Ada says whether `agent` may read her account, and is answered who
    /// may now.
    async fn name(&self, agent: &str, may: bool) -> Vec<String> {
        let saying = json!({"account": ACCOUNT, "agent": agent, "may": may});
        let said = self
            .ask(&self.ada(), person_accounts::ASSISTANT, saying)
            .await;
        read::<AccountAssistants>(said).agents
    }
}

/// What `caller` is answered when it asks `action` of `page`, or why not.
async fn asked_of(
    page: &(impl ActionHost + Sync),
    caller: &Caller,
    action: &str,
    argument: Value,
) -> Result<Value, ActionHostError> {
    let receipt = settled(page.invoke(caller, asking(action, argument))).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("the host answers at once, not {other:?}"),
    }
}

/// What an answer carries, read as `T`.
fn read<T: serde::de::DeserializeOwned>(answer: Result<Value, ActionHostError>) -> T {
    serde_json::from_value(answer.expect("the host answers")).expect("the answer reads")
}

/// `answer` is the one answer for unknown and forbidden.
fn nobody(what: &str, answer: Result<Value, ActionHostError>) {
    let refused = answer.expect_err(what);
    assert!(
        matches!(refused, ActionHostError::UnknownAction),
        "{what}: {refused:?}"
    );
}

/// The mail actions the catalogue offers `caller` on `page`: none when it
/// is offered nothing.
async fn offered(page: &(impl ActionHost + Sync), caller: &Caller) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(person_accounts::PROVIDER.into()),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    settled(page.query(caller, query))
        .await
        .map(|page| {
            page.rows
                .into_iter()
                .filter_map(|row| row.action.map(|action| action.address.action))
                .collect()
        })
        .unwrap_or_default()
}

/// A message from Ada to Bo, as a send begins one.
fn lunch() -> Value {
    json!({"message": {
        "account": ACCOUNT,
        "fromAddress": ACCOUNT,
        "to": "bo@example.test",
        "subject": "Lunch",
        "body": "Friday at one works.",
    }})
}

/// `caller` begins the message to Bo over `page` and asks for it to be
/// sent.
async fn send_lunch(
    page: &(impl ActionHost + Sync),
    caller: &Caller,
) -> Result<Value, ActionHostError> {
    let begun: StartedSend =
        read(asked_of(page, caller, person_accounts::BEGIN_SEND, lunch()).await);
    asked_of(
        page,
        caller,
        person_accounts::SEND,
        json!({"send": begun.send}),
    )
    .await
}

/// A person who answers every question one way, or not at all, and what
/// they were asked.
struct Asked {
    answer: Option<ConfirmationAnswer>,
    questions: Mutex<Vec<([u8; 32], PendingConfirmation)>>,
}

impl Asked {
    fn answering(answer: Option<ConfirmationAnswer>) -> Arc<Self> {
        Arc::new(Self {
            answer,
            questions: Mutex::default(),
        })
    }

    /// How many questions the person was put.
    fn count(&self) -> usize {
        self.questions.lock().unwrap().len()
    }
}

impl AsksPeople for Asked {
    fn ask(&self, person: [u8; 32], question: PendingConfirmation) -> PersonAnswer {
        self.questions.lock().unwrap().push((person, question));
        let answer = self.answer.ok_or_else(|| "nobody answered".to_owned());
        Box::pin(async move { answer })
    }
}

/// Only the person says which agent of theirs may read which account, on a
/// device of theirs. Until they do, their own agent is nobody to their mail
/// and is offered nothing; once they did, it is offered what it may do and
/// reads the account it was named for alone; and when they take it back, it
/// is nobody again.
#[tokio::test]
async fn a_person_names_the_agent_that_may_read_an_account() {
    let desk = Desk::new().await;
    let (ada, agent) = (desk.ada(), desk.agent(ASSISTANT));
    keep(
        &desk
            .workspaces
            .person_settings_root(&desk.device.user_public_key),
        "ada@other.test",
        "another-password",
    );
    let hers = || json!({"account": ACCOUNT});

    // Nobody is named yet: her own agent is nobody to her mail.
    let named = desk.ask(&ada, person_accounts::ASSISTANTS, hers()).await;
    assert!(read::<AccountAssistants>(named).agents.is_empty());
    let unnamed = desk.ask(&agent, person_accounts::LIST, json!({})).await;
    nobody("her agent before she named it", unnamed);
    assert!(offered(&desk.page(), &agent).await.is_empty());

    // An account she does not keep names nobody.
    let stranger = json!({"account": "eve@example.test", "agent": ASSISTANT, "may": true});
    let refused = desk.ask(&ada, person_accounts::ASSISTANT, stranger).await;
    assert!(
        matches!(refused, Err(ActionHostError::Refused { .. })),
        "{refused:?}"
    );

    assert_eq!(desk.name(ASSISTANT, true).await, [ASSISTANT]);
    let listed: MailAccounts = read(desk.ask(&agent, person_accounts::LIST, json!({})).await);
    let ids: Vec<&str> = listed.accounts.iter().map(|one| one.id.as_str()).collect();
    assert_eq!(ids, [ACCOUNT], "the account it was named for alone");
    let rows = offered(&desk.page(), &agent).await;
    for verb in READS_AND_DRAFTS {
        assert!(rows.iter().any(|row| row == verb), "{verb}: {rows:?}");
    }
    for verb in THE_PERSONS_ALONE {
        assert!(!rows.iter().any(|row| row == verb), "{verb}: {rows:?}");
    }
    let hers_directly = offered(&desk.page(), &ada).await;
    assert!(hers_directly.is_empty(), "a page asks the host directly");

    // Her other account, and another agent of hers.
    let other = json!({"account": "ada@other.test", "folder": "inbox"});
    let unread = desk.ask(&agent, person_accounts::FETCH, other).await;
    nobody("an account it was not named for", unread);
    let writer = desk.agent("writer");
    let unlisted = desk.ask(&writer, person_accounts::LIST, json!({})).await;
    nobody("an agent of hers she did not name", unlisted);

    // She takes it back.
    assert!(desk.name(ASSISTANT, false).await.is_empty());
    let after = desk.ask(&agent, person_accounts::LIST, json!({})).await;
    nobody("her agent after she took it back", after);
    assert!(offered(&desk.page(), &agent).await.is_empty());
    assert!(desk.servers.asked().is_empty(), "no server was asked");
}

/// What a named agent is offered among the mail actions.
const READS_AND_DRAFTS: [&str; 7] = [
    person_accounts::LIST,
    person_accounts::ARRIVED,
    person_accounts::FETCH,
    person_accounts::READ,
    person_accounts::SEARCH,
    person_accounts::SAVE_DRAFT,
    person_accounts::SEND,
];

/// What only the person does, on a device of theirs.
const THE_PERSONS_ALONE: [&str; 5] = [
    person_accounts::MARK,
    person_accounts::REMOVE,
    person_accounts::ADD_MAIL,
    person_accounts::ASSISTANT,
    person_accounts::ASSISTANTS,
];

/// The agent a person named reads a folder and a message, searches, hears
/// what arrived and keeps a draft, each through the host with the person's
/// own sign-in: over the connection a device of theirs certified, and in a
/// turn the host runs for them on such a device. What only the person does
/// is not there for it.
#[tokio::test]
async fn the_persons_assistant_reads_searches_and_drafts_through_the_host() {
    let desk = Desk::new().await;
    let agent = desk.agent(ASSISTANT);
    desk.name(ASSISTANT, true).await;
    let inbox = json!({"account": ACCOUNT, "folder": "inbox"});
    let message = json!({"account": ACCOUNT, "folder": "inbox", "uid": 4, "uidValidity": 7});

    let folder: FetchedMail = read(desk.ask(&agent, person_accounts::FETCH, inbox).await);
    assert_eq!(folder.messages[0].subject, "Lunch");
    let whole = desk
        .ask(&agent, person_accounts::READ, message.clone())
        .await;
    assert_eq!(read::<FetchedMessage>(whole).body, "Friday at one?");
    let search = json!({"account": ACCOUNT, "text": "lunch"});
    let found: FoundMail = read(desk.ask(&agent, person_accounts::SEARCH, search).await);
    assert_eq!(found.folders[0].messages[0].uid, 4);
    let arrived = desk.ask(&agent, person_accounts::ARRIVED, json!({})).await;
    assert!(read::<MailArrivals>(arrived).arrivals.is_empty());
    assert_eq!(desk.servers.running().len(), 1, "its asking is listening");
    let begun: StartedSend = read(desk.ask(&agent, person_accounts::BEGIN_SEND, lunch()).await);
    let keeping = json!({"send": begun.send});
    desk.ask(&agent, person_accounts::SAVE_DRAFT, keeping)
        .await
        .unwrap();

    // In a turn this host runs for her: her agent with a device of hers
    // behind it, as the host hands it the session.
    let in_turn = desk
        .ada()
        .delegated_to(ASSISTANT)
        .on_turn(Turn::new("chat/main", "message-1"), Vec::new());
    assert!(in_turn.turn().is_some(), "the turn is the caller's");
    let sent = json!({"account": ACCOUNT, "folder": "sent"});
    let turned = desk.ask(&in_turn, person_accounts::FETCH, sent).await;
    assert_eq!(read::<FetchedMail>(turned).folder, "sent");

    assert_eq!(
        desk.servers.asked(),
        [
            "fetch ada@example.test inbox",
            "read ada@example.test inbox 4",
            "search ada@example.test lunch",
            "draft ada@example.test to bo@example.test: Lunch",
            "fetch ada@example.test sent",
        ]
    );
    for action in THE_PERSONS_ALONE {
        nobody(action, desk.ask(&agent, action, message.clone()).await);
    }
}

/// Sending still asks. A message the person's assistant would send is put
/// to its person first: with nobody to ask through, or no answer, their
/// confirmation is still wanted and nothing leaves; a refusal sends
/// nothing; and only their allowing it sends the message, once. The person
/// sending their own message is asked nothing.
#[tokio::test]
async fn a_message_the_assistant_would_send_waits_for_its_person() {
    let desk = Desk::new().await;
    let page = desk.page();
    let agent = desk.agent(ASSISTANT);
    desk.name(ASSISTANT, true).await;

    let unasked = send_lunch(&page, &agent).await;
    assert!(
        matches!(unasked, Err(ActionHostError::ConfirmationRequired { .. })),
        "with nobody to ask through: {unasked:?}"
    );

    let silent = Asked::answering(None);
    desk.workspaces.ask_people(silent.clone());
    let unanswered = send_lunch(&page, &agent).await;
    assert!(
        matches!(
            unanswered,
            Err(ActionHostError::ConfirmationRequired { .. })
        ),
        "with no answer: {unanswered:?}"
    );
    let (person, question) = silent.questions.lock().unwrap()[0].clone();
    assert_eq!(person, desk.device.user_public_key, "her mail, her answer");
    assert_eq!(question.agent_id, ASSISTANT);
    assert_eq!(question.action_label, "send this message");
    assert_eq!(question.resource_label, ACCOUNT);
    for (label, value) in [
        ("To", "bo@example.test"),
        ("Subject", "Lunch"),
        ("Message", "Friday at one works."),
    ] {
        let shown = (label.to_owned(), value.to_owned());
        assert!(question.arguments.contains(&shown), "{label} is shown");
    }

    desk.workspaces
        .ask_people(Asked::answering(Some(ConfirmationAnswer::Deny)));
    let denied = send_lunch(&page, &agent).await;
    assert!(
        matches!(denied, Err(ActionHostError::Refused { .. })),
        "{denied:?}"
    );
    assert!(desk.servers.asked().is_empty(), "nothing left unallowed");

    let allowing = Asked::answering(Some(ConfirmationAnswer::AllowOnce));
    desk.workspaces.ask_people(allowing.clone());
    send_lunch(&page, &agent).await.unwrap();
    let sent = "send ada@example.test to bo@example.test: Lunch";
    assert_eq!(desk.servers.asked(), [sent]);
    assert_eq!(allowing.count(), 1, "one question for one message");

    send_lunch(&page, &desk.ada()).await.unwrap();
    assert_eq!(desk.servers.asked(), [sent, sent]);
    assert_eq!(allowing.count(), 1, "her own message asks her nothing");
}

/// A person's mail is theirs and their named agent's. Another person's
/// agent is nobody to it by any road: on its own person's device, over the
/// other's page, in a turn of its own person's, or under no person at all.
/// The person's own named agent is nobody too where no device of theirs
/// stands behind it, and in any workspace but the one it was named in. No
/// server is asked for any of them.
#[tokio::test]
async fn nobody_elses_agent_reads_a_persons_mail() {
    let desk = Desk::new().await;
    desk.name(ASSISTANT, true).await;
    let bo = key_of(&SigningKey::from_bytes(&rand::random()), "laptop");
    let bos_page = desk.host.in_workspace_as(desk.workspace_id, &bo);
    let bos_agent = Caller::delegated_agent(ASSISTANT, bo.user_public_key, bo.device_public_key);
    let bos_turn = Caller::agent_turn(
        ASSISTANT,
        hex::encode(bo.user_public_key),
        Turn::new("chat/main", "message-1"),
    );
    let line = desk.host.in_workspace(desk.workspace_id);
    let hers = || json!({"account": ACCOUNT, "folder": "inbox"});
    let fetch = person_accounts::FETCH;

    let own_device = asked_of(&bos_page, &bos_agent, fetch, hers()).await;
    nobody("on its own person's device", own_device);
    let listing = asked_of(&bos_page, &bos_agent, person_accounts::LIST, json!({})).await;
    nobody("listing on its own person's device", listing);
    nobody(
        "over the other person's page",
        desk.ask(&bos_agent, fetch, hers()).await,
    );
    nobody(
        "in a turn of its own person's",
        asked_of(&line, &bos_turn, fetch, hers()).await,
    );
    let unowned = Caller::local_agent(ASSISTANT);
    nobody("under no person", desk.ask(&unowned, fetch, hers()).await);
    // Her own named agent is hers only with a device of hers behind it,
    // and only in the workspace she named it in: its id means something
    // among that workspace's agents alone.
    let bare_turn = Caller::agent_turn(
        ASSISTANT,
        hex::encode(desk.device.user_public_key),
        Turn::new("chat/main", "message-2"),
    );
    nobody(
        "her agent's id with no device of hers behind it",
        asked_of(&line, &bare_turn, fetch, hers()).await,
    );
    let elsewhere = desk.workspaces.create("Shared").await.unwrap().workspace_id;
    let other_page = desk.host.in_workspace_as(elsewhere, &desk.device);
    nobody(
        "the same id in another workspace",
        asked_of(&other_page, &desk.agent(ASSISTANT), fetch, hers()).await,
    );
    assert!(offered(&bos_page, &bos_agent).await.is_empty());

    // Bo himself reads his own accounts, of which this is none.
    let his = asked_of(&bos_page, &caller(&bo), fetch, hers()).await;
    assert!(
        matches!(his, Err(ActionHostError::Refused { .. })),
        "{his:?}"
    );
    assert!(desk.servers.asked().is_empty(), "no server was asked");

    // The named agent of the person whose mail it is does read it.
    let own = desk.ask(&desk.agent(ASSISTANT), fetch, hers()).await;
    assert_eq!(read::<FetchedMail>(own).folder, "inbox");
}

/// Removing an account forgets who was named for it: the same address
/// kept again names nobody.
#[tokio::test]
async fn removing_an_account_forgets_the_agents_named_for_it() {
    let desk = Desk::new().await;
    let (ada, agent) = (desk.ada(), desk.agent(ASSISTANT));
    desk.name(ASSISTANT, true).await;
    let hers = || json!({"account": ACCOUNT});

    desk.ask(&ada, person_accounts::REMOVE, hers())
        .await
        .unwrap();
    let gone = desk.ask(&agent, person_accounts::LIST, json!({})).await;
    nobody("her agent once the account is gone", gone);

    keep(
        &desk
            .workspaces
            .person_settings_root(&desk.device.user_public_key),
        ACCOUNT,
        "adas-password",
    );
    let named = desk.ask(&ada, person_accounts::ASSISTANTS, hers()).await;
    assert!(read::<AccountAssistants>(named).agents.is_empty());
    let again = desk.ask(&agent, person_accounts::LIST, json!({})).await;
    nobody("her agent for the account kept again", again);
}
