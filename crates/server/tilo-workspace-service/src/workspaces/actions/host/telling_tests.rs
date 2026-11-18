//! A person's rule for what is important: the person says it in their own
//! words and reads it back; an agent of theirs reads it wherever it works
//! for them and writes it down only in a workspace of theirs; and nobody
//! else in a workspace they share reaches it.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host a test binds is kept until the test ends; dropping it earlier closes what it drives"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde_json::{Value, json};
use tilo_actions::remote::person_telling::{self, Decided, Importance, Reach};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, InvokeRequest, ReceiptStatus,
};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions;
use super::super::tests::{caller, settled};
use super::own_tests::{create_own, let_in, open};
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::TestHost;
use crate::workspaces::tests::{governor, guest_device, test_host};

pub(super) const ASSISTANT: &str = "assistant";

/// Ada's setup: a host that keeps workspaces of people's own, a device of
/// hers, the workspace that is her own, and one she shares with others.
pub(super) struct Setup {
    pub(super) workspaces: Workspaces,
    pub(super) host: ServiceWorkspaceActions,
    pub(super) ada: DeviceCert,
    pub(super) own: WorkspaceId,
    pub(super) shared: WorkspaceId,
    _server: TestHost,
    _dir: tempfile::TempDir,
}

impl Setup {
    pub(super) async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces().clone();
        let host = ServiceWorkspaceActions::new();
        host.attach(workspaces.clone());
        let (_, ada) = guest_device();
        let home = open(&workspaces).await;
        let_in(&workspaces, &home, ada.user_public_key).await;
        let created = create_own(&host, &ada, json!({"name": "Ada's workspace"}))
            .await
            .unwrap();
        let own = WorkspaceId::parse(&created.workspace.workspace).unwrap();
        assert!(workspaces.owns(&ada.user_public_key, &own.to_string()));
        let shared = workspaces.create("Team").await.unwrap().workspace_id;
        assert!(!workspaces.owns(&ada.user_public_key, &shared.to_string()));
        Self {
            workspaces,
            host,
            ada,
            own,
            shared,
            _server: server,
            _dir: dir,
        }
    }

    /// The page `device` holds in `workspace`, which a caller asks over.
    pub(super) fn page(
        &self,
        workspace: WorkspaceId,
        device: &DeviceCert,
    ) -> impl ActionProvider + Clone + Send + 'static {
        self.host.in_workspace_as(workspace, device)
    }

    /// What `caller` is answered over the page `device` holds in
    /// `workspace`.
    async fn ask(
        &self,
        (workspace, device): (WorkspaceId, &DeviceCert),
        caller: &Caller,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionCallError> {
        let page = self.page(workspace, device);
        let receipt = settled(page.invoke(caller, asking(action, argument))).await?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            other => panic!("the host answers at once, not {other:?}"),
        }
    }

    /// What Ada is answered on her device, in her own workspace.
    pub(super) async fn ada(
        &self,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionCallError> {
        self.ask((self.own, &self.ada), &caller(&self.ada), action, argument)
            .await
    }

    /// What Ada's agent is answered over her device, in `workspace`.
    async fn agent(
        &self,
        workspace: WorkspaceId,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionCallError> {
        self.ask(
            (workspace, &self.ada),
            &agent_of(&self.ada),
            action,
            argument,
        )
        .await
    }

    /// The actions of the rule the catalogue offers `caller` over Ada's
    /// page in `workspace`.
    async fn offered(&self, workspace: WorkspaceId, caller: &Caller) -> Vec<String> {
        let query = CatalogueQuery {
            provider: Some(person_telling::PROVIDER.into()),
            max_results: 100,
            ..CatalogueQuery::default()
        };
        let page = self.host.in_workspace_as(workspace, &self.ada);
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
}

/// The agent a person keeps, over the connection `device` certified.
pub(super) fn agent_of(device: &DeviceCert) -> Caller {
    Caller::delegated_agent(ASSISTANT, device.user_public_key, device.device_public_key)
}

/// A request of one action of the rule, with `argument` when it has one.
fn asking(action: &str, argument: Value) -> InvokeRequest {
    let mut request = super::accounts_tests::asking(action, argument);
    request.target = person_telling::target(person_telling::IMPORTANCE, action);
    request
}

/// What an answer carries, read as a rule.
fn rule(answer: Result<Value, ActionCallError>) -> Importance {
    serde_json::from_value(answer.expect("the host answers")).expect("the answer is a rule")
}

/// `answer` is the one answer for unknown and forbidden.
pub(super) fn nobody(what: &str, answer: Result<Value, ActionCallError>) {
    let refused = answer.expect_err(what);
    assert!(
        matches!(refused, ActionCallError::UnknownAction),
        "{what}: {refused:?}"
    );
}

/// Why `answer` was refused, in the words its asker reads.
fn refused(what: &str, answer: Result<Value, ActionCallError>) -> String {
    match answer.expect_err(what) {
        ActionCallError::Refused { reason } => reason,
        other => panic!("{what}: {other:?}"),
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one rule from the first line said to every way its person changes it"
)]
async fn a_person_says_what_is_important_in_their_own_words_and_reads_it_back() {
    let setup = Setup::new().await;

    // Nobody said anything yet: nothing is important by a rule.
    let empty = rule(setup.ada(person_telling::READ, json!({})).await);
    assert_eq!(empty, Importance::default());
    assert_eq!(empty.decided(), Decided::NoRule);

    let always = "My landlord is always important";
    let hiring = "A new customer is important only while we are hiring";
    rule(
        setup
            .ada(person_telling::SAY, json!({"words": always}))
            .await,
    );
    rule(
        setup
            .ada(
                person_telling::SAY,
                json!({"words": hiring, "onlyWhile": {"fact": "hiring", "is": "yes"}}),
            )
            .await,
    );
    let said = rule(
        setup
            .ada(
                person_telling::SET_FACT,
                json!({"name": "Hiring", "value": "no"}),
            )
            .await,
    );
    // Her words are kept as she said them, as hers, in the order said.
    let words: Vec<&str> = said.lines.iter().map(|line| line.words.as_str()).collect();
    assert_eq!(words, [always, hiring]);
    assert!(said.lines.iter().all(|line| line.by.is_empty()));
    assert_eq!(said.facts.len(), 1);
    assert_eq!(
        (said.facts[0].name.as_str(), said.facts[0].value.as_str()),
        ("Hiring", "no")
    );
    assert_eq!(said.revision, 3);
    // What she reads is what the host keeps, and what it decides by.
    assert_eq!(rule(setup.ada(person_telling::READ, json!({})).await), said);
    assert_eq!(
        setup.workspaces.importance_of(&setup.ada.user_public_key),
        said
    );
    assert_eq!(said.decided(), Decided::Judge(vec![&said.lines[0]]));

    // The same words said again are the same line.
    let again = rule(
        setup
            .ada(
                person_telling::SAY,
                json!({"words": hiring, "onlyWhile": {"fact": "hiring", "is": "yes"}}),
            )
            .await,
    );
    assert_eq!(again.lines.len(), 2);

    // With the line that always counts gone, the rule decides alone while
    // the situation stands as it does.
    let line = said.lines[0].id.clone();
    let waiting = rule(
        setup
            .ada(person_telling::UNSAY, json!({"line": line}))
            .await,
    );
    assert_eq!(
        waiting.decided(),
        Decided::NothingHolds(Some(("Hiring", "no")))
    );
    // A situation a line waits on is not forgotten from under it.
    let why = refused(
        "a situation a line waits on",
        setup
            .ada(person_telling::FORGET_FACT, json!({"name": "hiring"}))
            .await,
    );
    assert_eq!(why, "A line still depends on that situation");
    // The situation changes, and the same line counts.
    let hiring_now = rule(
        setup
            .ada(
                person_telling::SET_FACT,
                json!({"name": "hiring", "value": "yes"}),
            )
            .await,
    );
    assert_eq!(
        hiring_now.facts.len(),
        1,
        "a situation is set, not added twice"
    );
    assert!(matches!(hiring_now.decided(), Decided::Judge(lines) if lines.len() == 1));

    // How a thing that must not be missed repeats is hers to say, within
    // bounds.
    refused(
        "a reminder every five seconds",
        setup
            .ada(
                person_telling::SET_INSISTING,
                json!({"everySecs": 5, "times": 3}),
            )
            .await,
    );
    let insisting = rule(
        setup
            .ada(
                person_telling::SET_INSISTING,
                json!({"everySecs": 300, "times": 4}),
            )
            .await,
    );
    assert_eq!(
        (insisting.insists.every_secs, insisting.insists.times),
        (300, 4)
    );
    // Nothing breaks do-not-disturb and every workspace's words may be
    // pushed until she says otherwise.
    assert!(insisting.through_quiet.is_empty());
    assert!(insisting.carries_outside(&setup.shared.to_string()));
    let through = rule(
        setup
            .ada(
                person_telling::LET_THROUGH,
                json!({"kind": "reminder", "may": true}),
            )
            .await,
    );
    assert!(through.lets_through(Reach::Reminder));
    assert!(!through.lets_through(Reach::Important));
    let kept = rule(
        setup
            .ada(
                person_telling::KEEP_ON_HOST,
                json!({"workspace": setup.shared.to_string(), "kept": true}),
            )
            .await,
    );
    assert!(!kept.carries_outside(&setup.shared.to_string()));
    assert!(kept.carries_outside(&setup.own.to_string()));
    let back = rule(
        setup
            .ada(
                person_telling::LET_THROUGH,
                json!({"kind": "reminder", "may": false}),
            )
            .await,
    );
    assert!(!back.lets_through(Reach::Reminder));
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one agent asked each action from a workspace of its person and from a shared one"
)]
async fn a_persons_agent_reads_their_rule_and_writes_it_down_only_in_a_workspace_of_theirs() {
    let setup = Setup::new().await;
    let always = "My landlord is always important";
    rule(
        setup
            .ada(person_telling::SAY, json!({"words": always}))
            .await,
    );

    // In her own workspace her agent reads the rule and writes down what
    // she tells it, and the rule says who wrote it.
    let read = rule(
        setup
            .agent(setup.own, person_telling::READ, json!({}))
            .await,
    );
    assert_eq!(read.lines.len(), 1);
    let travelling = "Anything about the flight is important while I am travelling";
    let said = rule(
        setup
            .agent(
                setup.own,
                person_telling::SAY,
                json!({"words": travelling, "onlyWhile": {"fact": "travelling", "is": "yes"}}),
            )
            .await,
    );
    assert_eq!(said.lines[1].words, travelling);
    assert_eq!(said.lines[1].by, ASSISTANT);
    let set = rule(
        setup
            .agent(
                setup.own,
                person_telling::SET_FACT,
                json!({"name": "travelling", "value": "no"}),
            )
            .await,
    );
    assert_eq!(set.facts[0].by, ASSISTANT);
    // What her devices do with a notification is not her agent's to say.
    nobody(
        "an agent letting a kind through do-not-disturb",
        setup
            .agent(
                setup.own,
                person_telling::LET_THROUGH,
                json!({"kind": "important", "may": true}),
            )
            .await,
    );
    nobody(
        "an agent saying where a workspace's words may go",
        setup
            .agent(
                setup.own,
                person_telling::KEEP_ON_HOST,
                json!({"workspace": setup.own.to_string(), "kept": false}),
            )
            .await,
    );

    // In a workspace she shares, the same agent still reads her rule, and
    // changes nothing of it: what it reads there is other people's words.
    let read = rule(
        setup
            .agent(setup.shared, person_telling::READ, json!({}))
            .await,
    );
    assert_eq!(read, set);
    nobody(
        "an agent changing the rule from a shared workspace",
        setup
            .agent(
                setup.shared,
                person_telling::SAY,
                json!({"words": "Everything from this workspace is important"}),
            )
            .await,
    );
    nobody(
        "an agent setting a situation from a shared workspace",
        setup
            .agent(
                setup.shared,
                person_telling::SET_FACT,
                json!({"name": "travelling", "value": "yes"}),
            )
            .await,
    );
    nobody(
        "an agent removing a line from a shared workspace",
        setup
            .agent(
                setup.shared,
                person_telling::UNSAY,
                json!({"line": set.lines[0].id}),
            )
            .await,
    );
    assert_eq!(
        setup.workspaces.importance_of(&setup.ada.user_public_key),
        set
    );

    // The catalogue offers the agent exactly what it may do, and a page
    // of the person's own no row: it asks the host directly.
    let agent = agent_of(&setup.ada);
    assert_eq!(
        setup.offered(setup.own, &agent).await,
        [
            person_telling::READ,
            person_telling::SAY,
            person_telling::UNSAY,
            person_telling::SET_FACT,
            person_telling::FORGET_FACT,
            person_telling::SET_INSISTING,
            person_telling::TELL,
            person_telling::INSIST,
        ]
    );
    assert_eq!(
        setup.offered(setup.shared, &agent).await,
        [
            person_telling::READ,
            person_telling::TELL,
            person_telling::INSIST
        ]
    );
    assert!(
        setup
            .offered(setup.own, &caller(&setup.ada))
            .await
            .is_empty()
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "every kind of caller that is not the person or their own agent, each asked both ways"
)]
async fn nobody_else_reaches_a_persons_rule() {
    let setup = Setup::new().await;
    let always = "My landlord is always important";
    let said = rule(
        setup
            .ada(person_telling::SAY, json!({"words": always}))
            .await,
    );
    let (_, bo) = guest_device();

    // Another member's read is of their own rule, which says nothing: no
    // request names a person, so nothing of Ada's is theirs to ask for.
    for workspace in [setup.shared, setup.own] {
        let theirs = rule(
            setup
                .ask(
                    (workspace, &bo),
                    &caller(&bo),
                    person_telling::READ,
                    json!({}),
                )
                .await,
        );
        assert_eq!(theirs, Importance::default());
        // And so is their agent's.
        let theirs = rule(
            setup
                .ask(
                    (workspace, &bo),
                    &agent_of(&bo),
                    person_telling::READ,
                    json!({}),
                )
                .await,
        );
        assert_eq!(theirs, Importance::default());
    }
    // What another member says is their own rule's, not hers.
    rule(
        setup
            .ask(
                (setup.shared, &bo),
                &caller(&bo),
                person_telling::SAY,
                json!({"words": "Everything Bo sends is important"}),
            )
            .await,
    );
    assert_eq!(
        setup.workspaces.importance_of(&setup.ada.user_public_key),
        said
    );

    // An agent that says it acts for Ada over a device that is not hers
    // is nobody, whatever it asks.
    let pretending =
        Caller::delegated_agent(ASSISTANT, setup.ada.user_public_key, bo.device_public_key);
    for workspace in [setup.shared, setup.own] {
        nobody(
            "an agent naming a person its device is not certified for",
            setup
                .ask(
                    (workspace, &bo),
                    &pretending,
                    person_telling::READ,
                    json!({}),
                )
                .await,
        );
        nobody(
            "such an agent changing the rule",
            setup
                .ask(
                    (workspace, &bo),
                    &pretending,
                    person_telling::SAY,
                    json!({"words": "Everything Bo sends is important"}),
                )
                .await,
        );
    }
    // Nor is an agent with no device behind it, an app, or a bot installed
    // for her, even over her own device's connection.
    let strangers = [
        (
            "an agent no device stands behind",
            Caller::local_agent(ASSISTANT),
        ),
        ("an app", Caller::local_app("so.tilo.chat")),
        (
            "a bot installed for her",
            Caller::bot(
                "a-bot",
                setup.ada.user_public_key,
                setup.ada.device_public_key,
            ),
        ),
    ];
    for (what, stranger) in &strangers {
        nobody(
            what,
            setup
                .ask(
                    (setup.own, &setup.ada),
                    stranger,
                    person_telling::READ,
                    json!({}),
                )
                .await,
        );
        nobody(
            what,
            setup
                .ask(
                    (setup.own, &setup.ada),
                    stranger,
                    person_telling::SAY,
                    json!({"words": "Everything is important"}),
                )
                .await,
        );
        assert!(
            setup.offered(setup.own, stranger).await.is_empty(),
            "{what}"
        );
    }
    assert_eq!(
        setup.workspaces.importance_of(&setup.ada.user_public_key),
        said
    );
}
