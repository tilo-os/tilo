//! A workspace's mail, its members and their agents: every member reads the
//! accounts the workspace keeps; an agent a member's session was handed to
//! reads, searches and drafts through the host as the workspace's policy
//! lets it; a message it would send waits for its person; and nobody outside
//! the workspace is anybody to that mail.

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

use serde_json::{Value, json};
use tilo_action_policy::turn::Turn;
use tilo_actions::remote::asked::{AskedFacts, asked_person};
use tilo_actions::remote::person_accounts::{
    self, FetchedMail, FetchedMessage, FoundMail, MailAccounts, MailArrivals, StartedSend,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, InvokeRequest, ReceiptStatus,
};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions;
use super::super::tests::{admit, caller, governed, settled};
use super::accounts_tests::asking;
use super::turn_use_tests::{asked_by, asks, fact};
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::TestHost;
use crate::workspaces::mail_watch::testing::{Watched, keep};
use crate::workspaces::tests::{governor, test_host};

const ACCOUNT: &str = "ada@example.test";
const ASSISTANT: &str = "assistant";

/// Ada's setup: a holder whose mail servers answer and keep what they were
/// asked, the workspace she is a member of, a device of hers, and one account
/// the workspace keeps signed in.
struct Setup {
    servers: Arc<Watched>,
    workspaces: Workspaces,
    host: ServiceWorkspaceActions,
    owner: DeviceCert,
    workspace: String,
    device: DeviceCert,
    workspace_id: WorkspaceId,
    _server: TestHost,
    _dir: tempfile::TempDir,
}

impl Setup {
    /// [`Self::made`], boxed: the whole of it is too large a future to hold in place.
    async fn new() -> Self {
        Box::pin(Self::made()).await
    }

    async fn made() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces().clone();
        let servers = Arc::new(Watched::answering());
        workspaces.carry_mail_through(servers.clone());
        let (workspace, workspace_id, host) = governed(&workspaces).await;
        let device = admit(&workspaces, &workspace, &owner, 1).await;
        keep(
            &workspaces,
            &workspace_id.to_string(),
            ACCOUNT,
            "adas-password",
        )
        .await;
        Self {
            servers,
            workspaces,
            host,
            owner,
            workspace,
            device,
            workspace_id,
            _server: server,
            _dir: dir,
        }
    }

    /// Ada's page: what a device of hers asks the host through.
    fn page(&self) -> impl ActionProvider + Clone + Send + 'static {
        self.host.device_page(self.workspace_id, &self.device)
    }

    /// Ada, on her device.
    fn ada(&self) -> Caller {
        caller(&self.device)
    }

    /// The agent `name` Ada's device handed its session to, over the
    /// connection that device certified.
    fn agent(&self, name: &str) -> Caller {
        self.ada().delegated_to(name)
    }

    /// What `caller` is answered when it asks `action` over Ada's page, or, where it waits for
    /// her yes, what she is shown.
    async fn ask(
        &self,
        caller: &Caller,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionCallError> {
        self.waits(caller, &asking(action, argument)).await
    }

    /// `request` by `caller` over Ada's page: what it answered, or, where it waits in its asked
    /// record for her yes, what she is shown there.
    async fn waits(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let receipt = settled(self.page().invoke(caller, request.clone())).await?;
        super::waiting::at_once(&self.workspaces, request, receipt)
    }

    /// `action` asked by `caller` over Ada's page, which waits for its person's yes: the call,
    /// and what she is shown.
    async fn asks(
        &self,
        caller: &Caller,
        action: &str,
        argument: Value,
    ) -> (InvokeRequest, AskedFacts) {
        let request = asking(action, argument);
        let facts = asks(self.waits(caller, &request).await);
        (request, facts)
    }

    /// `request` by `caller`, which waits for Ada's yes, approved by her: what it answered.
    async fn approve(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        super::waiting::answered_by(
            &self.page(),
            &self.ada(),
            (caller, request),
            tilo_actions::remote::asked::Offer::Approve,
        )
        .await
    }

    /// `action` asked by `caller` over Ada's page and, where it waits for her yes, approved
    /// by her: what it answered, and what she was shown.
    async fn allowed(
        &self,
        caller: &Caller,
        action: &str,
        argument: Value,
    ) -> (Result<Value, ActionCallError>, Option<AskedFacts>) {
        let request = asking(action, argument);
        match self.waits(caller, &request).await {
            Err(ActionCallError::ConfirmationRequired(facts)) => {
                (self.approve(caller, &request).await, Some(*facts))
            }
            answer => (answer, None),
        }
    }
}

/// What `caller` is answered when it asks `action` of `page`, or why not.
pub(super) async fn asked_of(
    page: &(impl ActionProvider + Sync),
    caller: &Caller,
    action: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    answered(page, caller, &asking(action, argument)).await
}

/// `request` asked by `caller` over `page`, answered at once.
async fn answered(
    page: &(impl ActionProvider + Sync),
    caller: &Caller,
    request: &InvokeRequest,
) -> Result<Value, ActionCallError> {
    let receipt = settled(page.invoke(caller, request.clone())).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("the host answers at once, not {other:?}"),
    }
}

/// What an answer carries, read as `T`.
fn read<T: serde::de::DeserializeOwned>(answer: Result<Value, ActionCallError>) -> T {
    serde_json::from_value(answer.expect("the host answers")).expect("the answer reads")
}

/// `answer` is the one answer for unknown and forbidden.
fn nobody(what: &str, answer: Result<Value, ActionCallError>) {
    let refused = answer.expect_err(what);
    assert!(
        matches!(refused, ActionCallError::UnknownAction),
        "{what}: {refused:?}"
    );
}

/// The mail actions the catalogue offers `caller` on `page`: none when it
/// is offered nothing.
async fn offered(page: &(impl ActionProvider + Sync), caller: &Caller) -> Vec<String> {
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
pub(super) fn lunch() -> Value {
    json!({"message": {
        "account": ACCOUNT,
        "fromAddress": ACCOUNT,
        "to": "bo@example.test",
        "subject": "Lunch",
        "body": "Friday at one works.",
    }})
}

/// `caller` lists and fetches the workspace's one account over `page`.
async fn reads_the_account_kept(page: &(impl ActionProvider + Sync), caller: &Caller, who: &str) {
    let listed: MailAccounts = read(asked_of(page, caller, person_accounts::LIST, json!({})).await);
    let ids: Vec<&str> = listed.accounts.iter().map(|one| one.id.as_str()).collect();
    assert_eq!(ids, [ACCOUNT], "{who} reads the workspace's account");
    let inbox = json!({"account": ACCOUNT, "folder": "inbox"});
    let folder: FetchedMail = read(asked_of(page, caller, person_accounts::FETCH, inbox).await);
    assert_eq!(folder.messages[0].subject, "Lunch", "{who}");
}

/// Two members of one workspace read the same mail: what the workspace
/// keeps is listed and fetched for each, with its one sign-in, and an agent
/// a member's session was handed to is offered what it may do.
#[tokio::test]
async fn two_members_read_the_one_account_the_workspace_keeps() {
    let setup = Setup::new().await;
    let bo = admit(&setup.workspaces, &setup.workspace, &setup.owner, 2).await;
    let bos_page = setup.host.device_page(setup.workspace_id, &bo);
    reads_the_account_kept(&setup.page(), &setup.ada(), "Ada").await;
    reads_the_account_kept(&bos_page, &caller(&bo), "Bo").await;
    assert_eq!(
        setup.servers.asked(),
        [
            "fetch ada@example.test inbox",
            "fetch ada@example.test inbox"
        ]
    );
    let rows = offered(&setup.page(), &setup.agent(ASSISTANT)).await;
    for verb in READS_AND_DRAFTS {
        assert!(rows.iter().any(|row| row == verb), "{verb}: {rows:?}");
    }
    let hers_directly = offered(&setup.page(), &setup.ada()).await;
    assert!(hers_directly.is_empty(), "a page asks the host directly");
}

/// What an agent is offered among the mail actions.
const READS_AND_DRAFTS: [&str; 7] = [
    person_accounts::LIST,
    person_accounts::ARRIVED,
    person_accounts::FETCH,
    person_accounts::READ,
    person_accounts::SEARCH,
    person_accounts::SAVE_DRAFT,
    person_accounts::SEND,
];

/// What changes the workspace's mail, which a handed agent's person is asked
/// about first.
const CHANGES: [&str; 3] = [
    person_accounts::MARK,
    person_accounts::REMOVE,
    person_accounts::ADD_MAIL,
];

/// A member's agent reads a folder and a message, searches and hears what
/// arrived through the host with the workspace's sign-in: over the
/// connection a device of theirs certified, and in a turn the host runs for
/// them on such a device. A draft it keeps, and every change it would make,
/// waits for its person first.
#[tokio::test]
async fn a_members_agent_reads_and_searches_and_its_changes_ask_first() {
    let setup = Setup::new().await;
    let agent = setup.agent(ASSISTANT);
    let inbox = json!({"account": ACCOUNT, "folder": "inbox"});
    let message = json!({"account": ACCOUNT, "folder": "inbox", "uid": 4, "uidValidity": 7});

    let folder: FetchedMail = read(setup.ask(&agent, person_accounts::FETCH, inbox).await);
    assert_eq!(folder.messages[0].subject, "Lunch");
    let whole = setup
        .ask(&agent, person_accounts::READ, message.clone())
        .await;
    assert_eq!(read::<FetchedMessage>(whole).body, "Friday at one?");
    let search = json!({"account": ACCOUNT, "text": "lunch"});
    let found: FoundMail = read(setup.ask(&agent, person_accounts::SEARCH, search).await);
    assert_eq!(found.folders[0].messages[0].uid, 4);
    let arrived = setup.ask(&agent, person_accounts::ARRIVED, json!({})).await;
    assert!(read::<MailArrivals>(arrived).arrivals.is_empty());
    setup.workspaces.mail_watches_settled().await;
    assert_eq!(setup.servers.running().len(), 1, "its asking is listening");
    let begun: StartedSend = read(
        setup
            .ask(&agent, person_accounts::BEGIN_SEND, lunch())
            .await,
    );
    let keeping = json!({"send": begun.send});
    let (draft, _) = setup
        .asks(&agent, person_accounts::SAVE_DRAFT, keeping)
        .await;
    setup.approve(&agent, &draft).await.unwrap();

    // In a turn this host runs for her: her agent, certified by a device of hers.
    let in_turn = Caller::delegated_agent(
        ASSISTANT,
        setup.device.user_public_key,
        setup.device.device_public_key,
    )
    .on_roster(0, setup.device.user_public_key)
    .on_roster_turn(Turn::new("chat/main", "message-1"));
    assert!(in_turn.turn().is_some(), "the turn is the caller's");
    let sent = json!({"account": ACCOUNT, "folder": "sent"});
    let turned = setup.ask(&in_turn, person_accounts::FETCH, sent).await;
    assert_eq!(read::<FetchedMail>(turned).folder, "sent");

    assert_eq!(
        setup.servers.asked(),
        [
            "fetch ada@example.test inbox",
            "read ada@example.test inbox 4",
            "search ada@example.test lunch",
            "draft ada@example.test to bo@example.test: Lunch",
            "fetch ada@example.test sent",
        ]
    );
    // Each change waits for her, and nothing of one is done until she approves it.
    for action in CHANGES {
        setup.asks(&agent, action, message.clone()).await;
    }
    assert_eq!(setup.servers.asked().len(), 5, "no change was made");
}

/// Sending still asks. A message the person's assistant would send waits for its person,
/// showing them who it goes to and what it says; nothing leaves until they approve it, and
/// then it leaves once. The person's own message is asked of them as well, where the workspace
/// asks them before what leaves it.
#[tokio::test]
async fn a_message_the_assistant_would_send_waits_for_its_person() {
    let setup = Setup::new().await;
    let agent = setup.agent(ASSISTANT);

    let begun: StartedSend = read(
        setup
            .ask(&agent, person_accounts::BEGIN_SEND, lunch())
            .await,
    );
    let (sending, question) = setup
        .asks(&agent, person_accounts::SEND, json!({"send": begun.send}))
        .await;
    assert_eq!(
        asked_person(&agent),
        Some(hex::encode(setup.device.user_public_key)),
        "her agent, her answer"
    );
    assert!(asked_by(&question, ASSISTANT, ASSISTANT), "{question:?}");
    assert_eq!(question.verb, "send");
    assert_eq!(question.on.as_deref(), Some(ACCOUNT));
    for (label, value) in [
        ("To", "bo@example.test"),
        ("Subject", "Lunch"),
        ("Message", "Friday at one works."),
    ] {
        assert_eq!(fact(&question, label), value, "{label} is shown");
    }
    assert!(
        setup.servers.asked().is_empty(),
        "nothing leaves before she answers"
    );

    setup.approve(&agent, &sending).await.unwrap();
    let sent = "send ada@example.test to bo@example.test: Lunch";
    assert_eq!(setup.servers.asked(), [sent]);

    // Her own message is asked of her too, where the workspace asks an editor before what leaves
    // it: making the call is not her yes, her approve of its record is, and it leaves once.
    let ada = setup.ada();
    let hers: StartedSend = read(setup.ask(&ada, person_accounts::BEGIN_SEND, lunch()).await);
    let (sending, _) = setup
        .asks(&ada, person_accounts::SEND, json!({"send": hers.send}))
        .await;
    assert_eq!(setup.servers.asked(), [sent], "nothing left before her yes");
    setup.approve(&ada, &sending).await.unwrap();
    assert_eq!(setup.servers.asked(), [sent, sent]);
}

/// A workspace's mail is its members'. Somebody who is no member, and their
/// agent, is nobody to it by any road: on their own device, over a
/// member's page, in a turn of their own, or under no person at all. A
/// member of another workspace reads nothing of this one's mail there. No
/// server is asked for any of them.
#[tokio::test]
async fn nobody_outside_the_workspace_reads_its_mail() {
    let setup = Setup::new().await;
    let (elsewhere, elsewhere_id, _) = governed(&setup.workspaces).await;
    let bo = admit(&setup.workspaces, &elsewhere, &setup.owner, 2).await;
    let bos_page = setup.host.device_page(setup.workspace_id, &bo);
    let bos_agent = caller(&bo).delegated_to(ASSISTANT);
    let bos_turn = Caller::delegated_agent(ASSISTANT, bo.user_public_key, bo.device_public_key)
        .in_workspace(setup.workspace_id.to_string())
        .on_roster(0, bo.user_public_key)
        .on_roster_turn(Turn::new("chat/main", "message-1"));
    let line = setup.host.in_workspace(setup.workspace_id);
    let hers = || json!({"account": ACCOUNT, "folder": "inbox"});
    let fetch = person_accounts::FETCH;

    nobody(
        "a member of another workspace",
        asked_of(&bos_page, &caller(&bo), fetch, hers()).await,
    );
    nobody(
        "their agent on their own device",
        asked_of(&bos_page, &bos_agent, fetch, hers()).await,
    );
    let listing = asked_of(&bos_page, &bos_agent, person_accounts::LIST, json!({})).await;
    nobody("listing on their own device", listing);
    nobody(
        "over a member's page",
        setup.ask(&bos_agent, fetch, hers()).await,
    );
    nobody(
        "in a turn of their own",
        asked_of(&line, &bos_turn, fetch, hers()).await,
    );
    let unowned = Caller::local_agent(ASSISTANT);
    nobody("under no person", setup.ask(&unowned, fetch, hers()).await);
    assert!(offered(&bos_page, &bos_agent).await.is_empty());

    // In the workspace Bo is a member of, this account is none it keeps.
    let his_page = setup.host.device_page(elsewhere_id, &bo);
    let his: MailAccounts =
        read(asked_of(&his_page, &caller(&bo), person_accounts::LIST, json!({})).await);
    assert!(his.accounts.is_empty(), "{his:?}");
    let theirs = asked_of(&his_page, &caller(&bo), fetch, hers()).await;
    assert!(
        matches!(theirs, Err(ActionCallError::Refused { .. })),
        "{theirs:?}"
    );
    assert!(setup.servers.asked().is_empty(), "no server was asked");
}

/// A person moving in from another mail program connects their accounts from the computer they
/// logged in with `tilo login`, which asks them in its terminal first: the agent that computer
/// hands its own session to adds their accounts into the workspace once they allowed it,
/// signed in before anything is kept, and lists them as they do.
#[tokio::test]
async fn the_persons_own_computer_connects_an_account_they_move_in() {
    let setup = Setup::new().await;
    setup
        .workspaces
        .sign_in_accounts_through(|_, password| match password {
            "bos-password" => Ok("signed in".into()),
            _ => Err("IMAP server rejected the credential".into()),
        });
    let own_computer = setup
        .ada()
        .delegated_to(tilo_actions::remote::device_agent_id(
            &setup.device.user_public_key,
            "Migration",
        ));
    let adding = |password: &str| {
        json!({
            "address": "ada@lab.example.test",
            "displayName": "Ada Lovelace",
            "incomingHost": "imap.lab.example.test",
            "incomingPort": 993,
            "outgoingHost": "smtp.lab.example.test",
            "outgoingPort": 465,
            "password": password,
        })
    };

    let (refused, _) = setup
        .allowed(&own_computer, person_accounts::ADD_MAIL, adding("wrong"))
        .await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("rejected")),
        "{refused:?}"
    );
    let (added, asked) = setup
        .allowed(
            &own_computer,
            person_accounts::ADD_MAIL,
            adding("bos-password"),
        )
        .await;
    assert!(asked.is_some(), "she is asked before it is kept");
    let added = added.unwrap();
    assert_eq!(added["address"], "ada@lab.example.test", "{added}");
    let listed: MailAccounts = read(
        setup
            .ask(&own_computer, person_accounts::LIST, json!({}))
            .await,
    );
    assert!(
        listed
            .accounts
            .iter()
            .any(|account| account.address == "ada@lab.example.test"),
        "{listed:?}"
    );
}

/// Security review, 17 November 2026. The question a person is put before a
/// message their assistant wrote leaves must name everyone the message goes
/// to: what they allow is what they were shown. The question shows the first
/// 600 characters of each list of recipients and nothing says it was cut, so
/// an address written after a stretch of blanks is sent to and never shown.
#[tokio::test]
async fn the_question_names_every_address_the_message_goes_to() {
    let setup = Setup::new().await;
    let page = setup.page();
    let agent = setup.agent(ASSISTANT);

    let unseen = "eve@elsewhere.test";
    let to = format!("bo@example.test,{}{unseen}", " ".repeat(700));
    let message = json!({"message": {
        "account": ACCOUNT,
        "fromAddress": ACCOUNT,
        "to": to,
        "subject": "Lunch",
        "body": "Friday at one works.",
    }});
    let begun: StartedSend =
        read(asked_of(&page, &agent, person_accounts::BEGIN_SEND, message).await);
    let (left, question) = setup
        .allowed(&agent, person_accounts::SEND, json!({"send": begun.send}))
        .await;
    left.expect("the person allowed what they were shown");
    let question = question.expect("she was asked");

    // The message left, addressed to both.
    let left = setup.servers.asked();
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(
        left.iter().all(
            |sent| sent.starts_with("send ada@example.test to bo@example.test,")
                && sent.contains(unseen)
        ),
        "{left:?}"
    );

    // What the person was shown before they allowed it.
    let shown: Vec<(&str, &str)> = question
        .facts
        .iter()
        .map(|fact| (fact.label.as_str(), fact.value.trim_end()))
        .collect();
    assert!(
        shown.iter().any(|(_, value)| value.contains(unseen)),
        "the message went to {unseen}, and the question the person allowed never named it: {shown:?}"
    );
}

/// The lunch to Bo with `fields` of its message set anew.
pub(super) fn lunch_with(fields: &Value) -> Value {
    let mut begun = lunch();
    for (name, value) in fields.as_object().into_iter().flatten() {
        begun["message"][name.as_str()] = value.clone();
    }
    begun
}

/// Why a request was not carried out.
type Refusal = ActionCallError;

/// The rows a question names its recipients in.
const WHOM: [&str; 3] = ["To", "Copy to", "Blind copy to"];
/// What the lunch to Bo alone is handed to the servers as.
const LUNCH_LEFT: &str = "send ada@example.test to bo@example.test: Lunch";

/// Ada's setup with her assistant, and Ada allowing whatever she is asked:
/// so what leaves is what the question let through.
struct Sending {
    setup: Setup,
    /// What she was shown, question by question.
    shown: Mutex<Vec<AskedFacts>>,
}

impl Sending {
    /// [`Self::made`], boxed: the whole of it is too large a future to hold in place.
    async fn new() -> Self {
        Box::pin(Self::made()).await
    }

    async fn made() -> Self {
        Self {
            setup: Setup::new().await,
            shown: Mutex::default(),
        }
    }

    /// Her assistant begins `begun`, answered the id it is sent by.
    async fn begin(&self, begun: Value) -> Result<String, Refusal> {
        let agent = self.setup.agent(ASSISTANT);
        let started = self
            .setup
            .ask(&agent, person_accounts::BEGIN_SEND, begun)
            .await?;
        Ok(serde_json::from_value::<StartedSend>(started).unwrap().send)
    }

    /// Her assistant asks for the message begun as `send` to be sent, which she approves
    /// where it waits for her.
    async fn send_begun(&self, send: &str) -> Result<Value, Refusal> {
        let agent = self.setup.agent(ASSISTANT);
        let (answer, shown) = self
            .setup
            .allowed(&agent, person_accounts::SEND, json!({"send": send}))
            .await;
        self.shown.lock().unwrap().extend(shown);
        answer
    }

    /// How many questions she was put.
    fn count(&self) -> usize {
        self.shown.lock().unwrap().len()
    }

    /// Her assistant begins `begun` and asks for it to be sent.
    async fn send(&self, begun: Value) -> Result<Value, Refusal> {
        let send = self.begin(begun).await?;
        self.send_begun(&send).await
    }

    /// The values of the rows called `label` in the question she was put
    /// last, in its order.
    fn rows(&self, label: &str) -> Vec<String> {
        let shown = self.shown.lock().unwrap();
        let question = shown.last().expect("she was asked");
        question
            .facts
            .iter()
            .filter(|fact| fact.label == label)
            .map(|fact| fact.value.clone())
            .collect()
    }

    /// Every address the question she was put last names as a recipient.
    fn named(&self) -> Vec<String> {
        WHOM.iter().flat_map(|label| self.rows(label)).collect()
    }

    /// `answer` refused the message before she was asked anything, and
    /// nothing left: the reason it gave.
    fn refused_unasked(&self, what: &str, answer: Result<Value, Refusal>) -> String {
        let Err(Refusal::Refused { reason }) = answer else {
            panic!("{what} is refused, not {answer:?}");
        };
        assert_eq!(self.count(), 0, "{what}: she is asked nothing");
        let left = self.setup.servers.asked();
        assert!(left.is_empty(), "{what}: nothing leaves, not {left:?}");
        reason
    }
}

/// The address a message is handed to the server for is the one in angle
/// brackets, whatever stands before it. The question names that address and
/// nothing else: a name that reads as an address, quoted or not, with a
/// comma inside or not, hides nobody.
#[tokio::test]
async fn a_name_that_reads_as_an_address_hides_nobody() {
    let sending = Sending::new().await;
    let named_so = [
        "bo@example.test <eve@elsewhere.test>",
        "\"bo@example.test\" <eve@elsewhere.test>",
        "\"Bo, bo@example.test\" <eve@elsewhere.test>",
    ];
    for to in named_so {
        sending
            .send(lunch_with(&json!({"to": to})))
            .await
            .expect("she allowed what she was shown");
        assert_eq!(sending.named(), ["eve@elsewhere.test"], "{to}");
        assert_eq!(sending.rows("Goes to"), ["1 address"], "{to}");
    }
    let left = "send ada@example.test to eve@elsewhere.test: Lunch";
    assert_eq!(sending.setup.servers.asked(), [left, left, left]);
}

/// A group of addresses under one name is no list a person reads at a
/// glance: the message is refused before anyone is asked, and nothing
/// leaves.
#[tokio::test]
async fn a_group_of_addresses_is_refused_unasked() {
    let sending = Sending::new().await;
    for to in [
        "friends: bo@example.test, eve@elsewhere.test;",
        "bo@example.test, friends: Eve <eve@elsewhere.test>;",
    ] {
        let answer = sending.send(lunch_with(&json!({"to": to}))).await;
        sending.refused_unasked(to, answer);
    }
}

/// A line break, or any other character a line cannot hold, in what a
/// message's head is written from would begin a header of the writer's
/// own: a second recipient, or where its answers go. Such a message is
/// refused before anyone is asked, whichever field carries it.
#[tokio::test]
async fn a_line_break_in_a_field_is_refused_unasked() {
    let sending = Sending::new().await;
    let smuggled = "1@example.test>\r\nReply-To: <eve@elsewhere.test";
    for fields in [
        json!({"to": "bo@example.test\r\nBcc: eve@elsewhere.test"}),
        json!({"to": "bo@example.test, Bo <bo@example.test>\r\nReply-To: <eve@elsewhere.test>"}),
        json!({"cc": "cy@example.test\nBcc: eve@elsewhere.test"}),
        json!({"bcc": "dee@example.test\u{b}eve@elsewhere.test"}),
        json!({"subject": "Lunch\r\nReply-To: eve@elsewhere.test"}),
        json!({"fromName": "Ada\r\nReply-To: eve@elsewhere.test"}),
        json!({"fromAddress": "ada@example.test>\r\nReply-To: <eve@elsewhere.test"}),
        json!({"messageId": smuggled}),
        json!({"inReplyTo": smuggled}),
        json!({"references": ["0@example.test", smuggled]}),
    ] {
        let answer = sending.send(lunch_with(&fields)).await;
        sending.refused_unasked(&fields.to_string(), answer);
    }
}

/// An address in the subject or in the message is words, not a recipient:
/// the message goes to whom the question names and to nobody else, and the
/// question shows the subject whole.
#[tokio::test]
async fn an_address_in_the_subject_or_the_message_is_no_recipient() {
    let sending = Sending::new().await;
    let subject = "Lunch, cc eve@elsewhere.test";
    let fields = json!({"subject": subject, "body": "To: eve@elsewhere.test\r\n\r\nFriday."});
    sending
        .send(lunch_with(&fields))
        .await
        .expect("she allowed what she was shown");
    assert_eq!(sending.named(), ["bo@example.test"]);
    assert_eq!(sending.rows("Subject"), [subject]);
    assert_eq!(
        sending.setup.servers.asked(),
        [format!(
            "send ada@example.test to bo@example.test: {subject}"
        )]
    );
}

/// Whom it is sent as, every copy and every blind copy are named, each
/// address in a row of its own and all of them counted, however far into
/// its list an address stands; and they are what the servers are handed.
#[tokio::test]
async fn the_sender_and_every_copy_are_named() {
    let sending = Sending::new().await;
    let fields = json!({
        "fromName": "Ada L.",
        "fromAddress": "office@example.test",
        "cc": "Cy <cy@example.test>",
        "bcc": format!("dee@example.test;{}eve@elsewhere.test", " ".repeat(700)),
    });
    sending
        .send(lunch_with(&fields))
        .await
        .expect("she allowed what she was shown");
    assert_eq!(sending.rows("From"), ["office@example.test"]);
    assert_eq!(sending.rows("Sender name"), ["Ada L."]);
    assert_eq!(sending.rows("Goes to"), ["4 addresses"]);
    assert_eq!(sending.rows("To"), ["bo@example.test"]);
    assert_eq!(sending.rows("Copy to"), ["cy@example.test"]);
    assert_eq!(
        sending.rows("Blind copy to"),
        ["dee@example.test", "eve@elsewhere.test"]
    );
    assert_eq!(
        sending.setup.servers.asked(),
        [
            "send ada@example.test as office@example.test to bo@example.test \
          copy to cy@example.test blind copy to dee@example.test, eve@elsewhere.test: Lunch"
        ]
    );
}

/// A question shows twenty addresses. A message to as many is asked about
/// with each of them named; one to more is refused before anyone is asked,
/// with their number, until it names no more than can be shown.
#[tokio::test]
async fn more_addresses_than_a_question_shows_are_refused_with_their_number() {
    let sending = Sending::new().await;
    let people =
        |most: usize| -> Vec<String> { (1..=most).map(|n| format!("p{n}@example.test")).collect() };
    let too_many = json!({"to": people(15).join(", "), "cc": people(6).join(", ")});
    let answer = sending.send(lunch_with(&too_many)).await;
    let reason = sending.refused_unasked("twenty-one addresses", answer);
    assert!(
        reason.contains("21"),
        "it says how many there are: {reason}"
    );

    sending
        .send(lunch_with(&json!({"to": people(20).join(", ")})))
        .await
        .expect("she allowed what she was shown");
    assert_eq!(sending.rows("Goes to"), ["20 addresses"]);
    assert_eq!(sending.named(), people(20));
    assert_eq!(sending.setup.servers.asked().len(), 1);
}

/// An address its person could misread is not put to them at all: one
/// whose first part is quoted and holds another address, one with letters
/// that only look like the ones they know, one with a character that has no
/// width, and one too long to stand in a row. Each is refused before
/// anyone is asked.
#[tokio::test]
async fn an_address_that_cannot_be_read_plainly_is_refused_unasked() {
    let sending = Sending::new().await;
    let long = format!("bo@{}example.test", "a.".repeat(60));
    for to in [
        "\"bo@example.test, x\"@elsewhere.test",
        "b\u{43e}@example.test",
        "bo\u{200b}@example.test",
        "bo@ex\u{430}mple.test",
        long.as_str(),
    ] {
        let answer = sending.send(lunch_with(&json!({"to": to}))).await;
        sending.refused_unasked(&format!("{to:?}"), answer);
    }
}

/// What a question does not show it says, in a row of its own that no
/// message can write: a long message is shown by its start with the number
/// of characters that follow, and so is a long subject. Blank lines before
/// a message's first word use up none of what is shown.
#[tokio::test]
async fn what_a_question_does_not_show_it_says() {
    let sending = Sending::new().await;
    let start = "Friday at one works. ".repeat(40);
    let body = format!("{start}Send the plans to Eve.");
    let subject = format!("Lunch {}", "and more ".repeat(40));
    sending
        .send(lunch_with(&json!({"subject": subject, "body": body})))
        .await
        .expect("she allowed what she was shown");
    let shown: String = body.chars().take(600).collect();
    assert_eq!(sending.rows("Message"), [shown]);
    assert_eq!(
        sending.rows("Message continues"),
        ["262 more characters are not shown here"]
    );
    let shown: String = subject.trim_end().chars().take(200).collect();
    assert_eq!(sending.rows("Subject"), [shown]);
    assert_eq!(
        sending.rows("Subject continues"),
        ["165 more characters are not shown here"]
    );

    let pushed_down = format!("{}Bring the plans.", "\n".repeat(700));
    sending
        .send(lunch_with(&json!({"body": pushed_down})))
        .await
        .expect("she allowed what she was shown");
    assert_eq!(sending.rows("Message"), ["Bring the plans."]);
    assert!(sending.rows("Message continues").is_empty());
}

/// A second version of the message that its reader's program would show
/// in place of the words the person was shown is not sent for an
/// assistant: the message is refused before anyone is asked.
#[tokio::test]
async fn a_version_the_person_is_not_shown_is_refused_unasked() {
    let sending = Sending::new().await;
    let fields = json!({"htmlBody": "<p>Send the plans to <b>Eve</b>.</p>"});
    let answer = sending.send(lunch_with(&fields)).await;
    sending.refused_unasked("a formatted version", answer);
}

/// One outbox message is sent once, whatever asks the workspace's server for it: the
/// message the workspace's outbox holds is named by its own id, and a send
/// the workspace already kept answers that send instead of handing the mail
/// to its account's server again.
#[tokio::test]
async fn a_message_the_workspace_holds_leaves_once() {
    let sending = Sending::new().await;
    let begun = lunch_with(&json!({"messageId": "outbox-1@example.test"}));
    let first = sending.send(begun.clone()).await.expect("it leaves");
    let asked = sending.count();
    let again = sending
        .send(begun)
        .await
        .expect("the send already made answers");
    assert_eq!(first, again, "the same send is answered");
    assert_eq!(
        sending.setup.servers.asked(),
        [LUNCH_LEFT],
        "the server is handed the message once"
    );
    assert_eq!(
        sending.count(),
        asked,
        "its person is not asked about a message that left"
    );
}

/// A file and an invitation a message carries are named with their size,
/// and the person is asked only once every file is whole: what they allow
/// is then what leaves.
#[tokio::test]
async fn files_and_an_invitation_are_named_once_they_are_whole() {
    let sending = Sending::new().await;
    let invitation = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n";
    let mut begun = lunch();
    begun["calendarIcs"] = json!(invitation);
    begun["files"] = json!([{"filename": "notes.txt", "mime": "text/plain", "size": 5}]);
    let send = sending.begin(begun).await.unwrap();

    let early = sending.send_begun(&send).await;
    sending.refused_unasked("a message whose file has not arrived", early);

    let agent = sending.setup.agent(ASSISTANT);
    let piece = json!({"send": send, "file": 0, "offset": 0, "bytes": hex::encode("hello")});
    sending
        .setup
        .ask(&agent, person_accounts::SEND_PIECE, piece)
        .await
        .unwrap();
    sending
        .send_begun(&send)
        .await
        .expect("she allowed what she was shown");
    assert_eq!(sending.rows("File"), ["notes.txt (5 bytes)"]);
    assert_eq!(
        sending.rows("Invitation"),
        [format!(
            "A calendar invitation of {} characters is attached and not shown here",
            invitation.chars().count()
        )]
    );
    assert_eq!(sending.setup.servers.asked(), [LUNCH_LEFT]);
}

/// What leaves is what its person was shown: a begun message written anew where it is kept,
/// after they were asked and before their approve, leaves on that approve neither as it was
/// shown nor as it was rewritten.
#[tokio::test]
async fn a_message_written_anew_after_it_was_shown_does_not_leave() {
    let setup = Setup::new().await;
    let agent = setup.agent(ASSISTANT);
    let begun: StartedSend = read(
        setup
            .ask(&agent, person_accounts::BEGIN_SEND, lunch())
            .await,
    );
    let (sending, question) = setup
        .asks(&agent, person_accounts::SEND, json!({"send": begun.send}))
        .await;
    assert_eq!(fact(&question, "To"), "bo@example.test");
    let sends = setup
        .workspaces
        .workspace_root(&setup.workspace_id.to_string())
        .join("sends");
    for begun in std::fs::read_dir(&sends).unwrap().flatten() {
        let meta = begun.path().join("meta.json");
        let mut kept: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
        kept["message"]["to"] = json!("eve@elsewhere.test");
        kept["message"]["subject"] = json!("Plans");
        std::fs::write(&meta, serde_json::to_vec(&kept).unwrap()).unwrap();
    }

    let refused = setup.approve(&agent, &sending).await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains("not what you were shown")),
        "{refused:?}"
    );
    assert!(setup.servers.asked().is_empty(), "nothing left");
}

/// The account `tilo migrate` adds, asked through the workspace's own governance action
/// (`workspace/mail-accounts#add`) as the device agent its run is.
fn account_asking(setup: &Setup, adding: Value) -> InvokeRequest {
    use tilo_actions::remote::{
        WORKSPACE_GOVERNANCE_PROVIDER, WORKSPACE_MAIL_ACCOUNTS, workspace_governance_child,
    };
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: WORKSPACE_GOVERNANCE_PROVIDER.into(),
            address: tilo_actions::ActionAddress::new(
                workspace_governance_child(&setup.workspace, Some(WORKSPACE_MAIL_ACCOUNTS)),
                "add",
            ),
        },
        argument: tilo_actions::remote::InvokeArgument::Json(
            adding
                .as_object()
                .expect("an account reads as its fields")
                .clone(),
        ),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The addresses the workspace keeps.
async fn kept_accounts(setup: &Setup) -> Vec<String> {
    let listed: MailAccounts = read(
        setup
            .ask(&setup.ada(), person_accounts::LIST, json!({}))
            .await,
    );
    listed
        .accounts
        .into_iter()
        .map(|account| account.address)
        .collect()
}

/// A migration adds an account through `workspace/mail-accounts#add`, as the device agent its
/// run is. The workspace's policy asks its person first, never silently allowing or denying:
/// nothing is kept until they answer, their no leaves the account unconnected, and their yes
/// keeps it.
#[tokio::test]
async fn a_migration_agent_adding_a_mail_account_asks_its_person_first() {
    let setup = Setup::new().await;
    setup
        .workspaces
        .sign_in_accounts_through(|_, password| match password {
            "bos-password" => Ok("signed in".into()),
            _ => Err("IMAP server rejected the credential".into()),
        });
    let agent = tilo_actions::remote::device_agent_id(&setup.device.user_public_key, "migrate");
    let migration = setup.ada().delegated_to(agent.clone());
    let address = "ada@lab.example.test";
    let adding = json!({
        "address": address,
        "displayName": "Ada Lovelace",
        "incomingHost": "imap.lab.example.test",
        "incomingPort": 993,
        "outgoingHost": "smtp.lab.example.test",
        "outgoingPort": 465,
        "password": "bos-password",
    });
    let request = account_asking(&setup, adding);

    // The policy answers Ask, naming the agent: nothing is kept before the person answers.
    let facts = asks(setup.waits(&migration, &request).await);
    assert!(
        matches!(&facts.by, tilo_actions::remote::asked::AskedBy::Agent(named) if named.id == agent),
        "{facts:?}"
    );
    assert!(facts.verb.contains("add"), "{facts:?}");
    assert!(
        !kept_accounts(&setup).await.contains(&address.to_owned()),
        "nothing is kept before they answer"
    );

    // Their no is no answer: the account stays unconnected.
    assert!(
        !kept_accounts(&setup).await.contains(&address.to_owned()),
        "their no leaves it unconnected"
    );

    // Their yes admits the same call again, and the account is kept, signed in first.
    let added = setup
        .approve(&migration, &request)
        .await
        .expect("their yes keeps it");
    assert_eq!(added["address"], address, "{added}");
    assert!(kept_accounts(&setup).await.contains(&address.to_owned()));
}
