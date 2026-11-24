//! A calendar connected by a member's agent telling the server where it is.
//! The turn of a member's agent hands over the address, the user name and a
//! name, and nothing else: no argument is a password, and none makes the
//! account writable. The call waits for the member, who types the password into their
//! own vault through its add, and no calendar server is asked anything before they
//! approve. Their approve connects the account for reading only, once, for the whole
//! workspace. A calendar server that refuses the sign-in keeps nothing here, and the
//! login stays theirs, in their vault. An address that is not https or is inside is
//! refused before anybody is asked. Told twice it adds nothing. And no turn that is not a
//! member's own agent's is offered it.

use super::super::super::turn_use_tests::{approved, asked_by, asks, dispatched, fact};
use super::super::super::vault_tests::ask as of_her_vault;
use super::*;
use crate::workspaces::hosted_use::Connected;
use tilo_actions::remote::asked::AskedFacts;

/// What she calls the account.
const NAME: &str = "My own server";
/// What she types when it is not her password. Made up, as every sign-in
/// here.
const MISTYPED: &str = "marker-2c9d-not-her-calendar-password";

impl Hers {
    /// What she told her assistant: where her calendar is, under which user
    /// name, and what she calls it.
    fn telling(&self) -> Value {
        json!({"serverUrl": self.server.url(), "username": ADDRESS, "name": NAME})
    }

    /// Where a sign-in for her calendar server belongs: its origin.
    fn place(&self) -> String {
        self.server.url().trim_end_matches("/dav/").to_owned()
    }

    /// The request a turn tells of the calendar `argument` names with.
    fn telling_request(argument: Value) -> InvokeRequest {
        let Value::Object(argument) = argument else {
            panic!("arguments are an object");
        };
        InvokeRequest {
            invocation: InvocationId::new(),
            target: Self::target(held::RESOURCE, held::CONNECT),
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

    /// What `turn` is answered when it tells of the calendar `argument`
    /// names.
    async fn tells(
        &self,
        turn: &HostedTurn<'_>,
        argument: Value,
    ) -> Result<Connected, ActionError> {
        connected(dispatched(&self.workspaces, turn, &Self::telling_request(argument)).await)
    }

    /// `turn` tells of her calendar, and it waits for her: the call, and what she is shown.
    async fn is_asked(&self, turn: &HostedTurn<'_>) -> (InvokeRequest, AskedFacts) {
        let request = Self::telling_request(self.telling());
        let facts = asks(dispatched(&self.workspaces, turn, &request).await);
        (request, facts)
    }

    /// One action of her vault, asked from her own device.
    async fn of_vault(&self, action: &str, argument: Value) -> Value {
        of_her_vault(
            &self.service,
            self.workspace_id,
            (&self.ada, &caller(&self.ada)),
            action,
            argument,
        )
        .await
        .expect("her vault answers her own device")
    }

    /// She types `password` into her vault's own add, as the asking view opens it filled from
    /// what `facts` show: the reference it is kept under.
    async fn types(&self, facts: &AskedFacts, password: &str) -> String {
        let keeping = json!({
            "kind": fact(facts, "kind"),
            "belongsTo": [fact(facts, "belongsTo")],
            "username": fact(facts, "username"),
            "secret": password,
        });
        let kept = self.of_vault(person_vault::ADD, keeping).await;
        kept["reference"].as_str().expect("a reference").to_owned()
    }

    /// The items her vault keeps, as her own device lists them.
    async fn vault(&self) -> Vec<Value> {
        let kept = self.of_vault(person_vault::LIST, json!({})).await;
        kept["items"].as_array().expect("her items").clone()
    }

    /// Her calendar accounts, as her own device lists them.
    async fn accounts(&self) -> Vec<Value> {
        let listed = self
            .herself(Self::target(held::RESOURCE, held::LIST), json!({}))
            .await;
        listed["accounts"].as_array().expect("her accounts").clone()
    }

    /// The connecting `turn` is offered, by its action's id.
    async fn connecting_offered(&self, turn: &HostedTurn<'_>) -> Vec<String> {
        let rows = self.workspaces.calendar_connect_offered(turn).await;
        rows.into_iter()
            .filter_map(|row| row.action.map(|action| action.address.action))
            .collect()
    }

    /// What comes of her approve of `request`, the call that waited for her.
    async fn answered(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Connected, ActionError> {
        let approved = approved(request, &self.workspace);
        connected(dispatched(&self.workspaces, turn, &approved).await)
    }
}

/// What a connecting the dispatch answered connected.
fn connected(answer: Result<Value, ActionError>) -> Result<Connected, ActionError> {
    answer.map(|value| {
        serde_json::from_value(value).expect("the dispatch answers what was connected")
    })
}

/// The words `answer` refuses a telling in.
fn in_words(what: &str, answer: Result<Connected, ActionError>) -> String {
    match answer {
        Err(ActionError::Refused { reason }) => reason,
        other => panic!("{what}: {other:?}"),
    }
}

/// A member's agent's turn tells of their calendar: it waits for them, showing the login
/// their vault is to keep at their calendar server, and nobody else is asked anything. What
/// they type, approved, connects the account for reading only, every calendar of it
/// too, in the workspace: the turn then reads what the calendar holds, as
/// any member's agent's does, and what would write to the calendar server is
/// refused in words also when they ask themselves. The turn is told the
/// calendars by their names and nothing else. The password is in no answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_they_tell_is_asked_for_and_their_typing_connects_it_for_reading_only() {
    let hers = Hers::new().await;
    // The test's calendar server answers on this machine: its operator lets
    // this server call its own network.
    hers.workspaces.call_private_model_addresses(true);
    hers.server.add_calendar("family", "Family");
    let scout = on_turn(&hers.ada, &hers.workspace, SCOUT);
    let turn = hers.turn(&scout, None);

    assert_eq!(
        hers.connecting_offered(&turn).await,
        [held::CONNECT],
        "her agent's turn is offered it before any account is kept"
    );
    assert!(
        hers.offered(&turn).await.is_empty(),
        "and nothing to read yet"
    );
    let (telling, facts) = hers.is_asked(&turn).await;
    assert_eq!(
        (fact(&facts, "belongsTo"), fact(&facts, "username")),
        (hers.place().as_str(), ADDRESS),
        "she is asked for a login at her calendar server, under her user name"
    );
    assert_eq!(fact(&facts, "kind"), "login");
    assert_eq!(
        facts.answered_by.as_deref(),
        Some(
            format!(
                "{}/{}#{}",
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::ADD
            )
            .as_str()
        ),
        "her answer is her own add to her vault, which no agent reads"
    );
    assert!(asked_by(&facts, SCOUT, "Scout"), "{facts:?}");
    let words = &facts.words;
    assert!(
        words.contains("reading only") && words.contains("Scout") && words.contains(ADDRESS),
        "the question says what her typing does: {words}"
    );
    assert!(
        hers.server.requests().is_empty(),
        "her calendar server is asked nothing before she typed"
    );
    assert!(hers.accounts().await.is_empty(), "and nothing is kept yet");

    hers.types(&facts, PASSWORD).await;
    let connected = hers
        .answered(&turn, &telling)
        .await
        .expect("her calendar server takes the sign-in");
    let account = format!(
        "caldav:{ADDRESS}@{}",
        hers.place().trim_start_matches("http://")
    );
    assert_eq!(
        serde_json::to_value(&connected).unwrap(),
        json!({"account": account, "name": NAME, "calendars": ["Family", "Work"]}),
        "the turn is told the calendars by their names, and nothing else"
    );

    let accounts = hers.accounts().await;
    assert_eq!(accounts.len(), 1, "{accounts:?}");
    assert_eq!(accounts[0]["id"], account.as_str(), "{accounts:?}");
    assert_eq!(accounts[0]["readOnly"], true, "{accounts:?}");
    assert!(
        accounts[0]["calendars"]
            .as_array()
            .expect("its calendars")
            .iter()
            .all(|calendar| calendar["readOnly"] == true),
        "every calendar of it is one that is not written: {accounts:?}"
    );
    assert!(
        !format!("{facts:?} {connected:?} {accounts:?}").contains(PASSWORD),
        "never the sign-in"
    );

    // The turn that asked reads the account it connected, as any member's
    // agent's turn reads the workspace's calendars.
    assert_eq!(
        hers.offered(&turn).await,
        [held::LIST, held::ENTRIES, held::FREE]
    );
    let read = hers
        .asks(&turn, held::ENTRIES, week())
        .await
        .expect("its turn reads what it connected");
    assert_eq!(read["entries"][0]["title"], "Review", "{read}");

    // Nothing is written to it, also when she asks herself.
    let page = hers.service.in_workspace_as(hers.workspace_id, &hers.ada);
    let lunch = carried(
        "REQUEST",
        &inviting(
            "lunch-1@example.test",
            "Lunch",
            "20261119T120000",
            "bo@elsewhere.test",
            &[ADDRESS],
        ),
    );
    let why = refused(
        "her own asking to put an entry in",
        asked(
            &page,
            &caller(&hers.ada),
            Hers::target(held::RESOURCE, held::ADD_INVITATION),
            json!({"ics": lunch}),
        )
        .await,
    );
    assert!(why.contains("reading only"), "{why}");
    assert!(hers.asked_only_to_read("connecting it and reading it") > 0);

    // What she typed is an item of her vault.
    assert_eq!(hers.vault().await.len(), 1);
}

/// A telling names an address, a user name and a name, and nothing else: no
/// argument is a password or a vault item, and none says how the account is
/// connected. Each such telling is refused as arguments that do not read,
/// and asks nobody.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_telling_takes_no_password_and_nothing_that_makes_the_account_writable() {
    let hers = Hers::new().await;
    hers.workspaces.call_private_model_addresses(true);
    let scout = on_turn(&hers.ada, &hers.workspace, SCOUT);
    let turn = hers.turn(&scout, None);
    for (name, value) in [
        ("readOnly", json!(false)),
        ("readOnly", json!(true)),
        ("read_only", json!(false)),
        ("password", json!("made-up")),
        ("item", json!("vault.item:0123456789abcdef0123456789abcdef")),
        ("calendars", json!([])),
    ] {
        let mut telling = hers.telling();
        telling[name] = value;
        let told = hers.tells(&turn, telling).await;
        assert!(
            matches!(told, Err(ActionError::InvalidRequest { .. })),
            "{name}: {told:?}"
        );
    }
    let nameless = hers
        .tells(
            &turn,
            json!({"serverUrl": hers.server.url(), "username": "  "}),
        )
        .await;
    assert!(
        in_words("no user name", nameless).contains("user name"),
        "a calendar account has a user name"
    );
    assert!(hers.server.requests().is_empty());
}

/// An address that is not https, that carries a sign-in or that is inside
/// is refused in words before anybody is asked, by a server that calls
/// public addresses alone, as a server does by default: the test's own
/// calendar server on this machine among them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_address_that_is_not_https_or_is_inside_is_refused_before_anybody_is_asked() {
    let hers = Hers::new().await;
    // The shared harness keeps its calendar server on this machine, so its
    // operator lets the server call its own network. This test is of a
    // server that calls public addresses alone, as a server does by default.
    hers.workspaces.call_private_model_addresses(false);
    let scout = on_turn(&hers.ada, &hers.workspace, SCOUT);
    let turn = hers.turn(&scout, None);
    for (address, words) in [
        (hers.server.url(), "https"),
        ("http://calendar.example.test/dav/".to_owned(), "https"),
        ("https://127.0.0.1:8443/dav/".to_owned(), "public"),
        ("https://10.0.0.8/dav/".to_owned(), "public"),
        ("https://[::1]/dav/".to_owned(), "public"),
        (
            "https://ada:made-up@calendar.example.test/".to_owned(),
            "sign-in",
        ),
    ] {
        let told = hers
            .tells(&turn, json!({"serverUrl": address, "username": ADDRESS}))
            .await;
        let why = in_words(&address, told);
        assert!(why.contains(words), "{address}: {why}");
    }
    assert!(
        hers.server.requests().is_empty(),
        "no calendar server is asked anything"
    );
}

/// A calendar server that refuses the sign-in is said so in its own words.
/// Nothing is kept here: no account, nothing to read. The logins in their vault are theirs
/// and stay, the one they kept before they told of it and the one they typed for the
/// question alike, for them to change or remove.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_calendar_server_that_refuses_the_password_keeps_no_item_and_no_account() {
    let hers = Hers::new().await;
    hers.workspaces.call_private_model_addresses(true);
    let scout = on_turn(&hers.ada, &hers.workspace, SCOUT);
    let turn = hers.turn(&scout, None);

    // A login she kept before, which her calendar server does not take.
    let before = hers
        .of_vault(
            person_vault::ADD,
            json!({
                "kind": "login", "belongsTo": [hers.place()], "username": ADDRESS,
                "secret": MISTYPED,
            }),
        )
        .await;
    let before = before["reference"]
        .as_str()
        .expect("a reference")
        .to_owned();
    let (telling, _) = hers.is_asked(&turn).await;
    let why = in_words(
        "her calendar server refuses it",
        hers.answered(&turn, &telling).await,
    );
    assert!(
        why.contains("rejected"),
        "in her calendar server's words: {why}"
    );
    assert!(!before.is_empty());
    assert_eq!(
        hers.vault().await.len(),
        1,
        "what she kept before the question stays hers"
    );

    // What she types for the question is a login of her vault, as any she keeps.
    let (telling, facts) = hers.is_asked(&turn).await;
    hers.types(&facts, MISTYPED).await;
    assert_eq!(hers.vault().await.len(), 2);
    let why = in_words(
        "her calendar server refuses it",
        hers.answered(&turn, &telling).await,
    );
    assert!(why.contains("rejected"), "{why}");
    assert_eq!(hers.vault().await.len(), 2, "her vault is hers to change");
    assert!(hers.accounts().await.is_empty(), "no account is kept");
    assert!(hers.offered(&turn).await.is_empty(), "nothing to read");
    assert!(
        hers.server.mailed().is_empty(),
        "her calendar server wrote to nobody"
    );
}

/// Where she declines, the call is never admitted again: nothing is connected and her
/// calendar server is asked nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declining_connects_nothing() {
    let hers = Hers::new().await;
    hers.workspaces.call_private_model_addresses(true);
    let scout = on_turn(&hers.ada, &hers.workspace, SCOUT);
    let turn = hers.turn(&scout, None);

    hers.is_asked(&turn).await;
    assert!(hers.accounts().await.is_empty());
    assert!(hers.server.requests().is_empty());
}

/// Told of the same account while she has not answered, it waits again. Told of it once it
/// is connected, nothing is added and nobody is asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_account_told_twice_asks_once_and_adds_nothing_once_connected() {
    let hers = Hers::new().await;
    hers.workspaces.call_private_model_addresses(true);
    let scout = on_turn(&hers.ada, &hers.workspace, SCOUT);
    let turn = hers.turn(&scout, None);

    let (telling, facts) = hers.is_asked(&turn).await;
    let (_, again) = hers.is_asked(&turn).await;
    assert_eq!(again, facts, "it waits again, as it was");

    hers.types(&facts, PASSWORD).await;
    hers.answered(&turn, &telling)
        .await
        .expect("her calendar server takes the sign-in");
    let why = in_words(
        "told of it once more",
        hers.tells(&turn, hers.telling()).await,
    );
    assert!(why.contains("connected already"), "{why}");
    assert_eq!(hers.accounts().await.len(), 1);
    assert_eq!(hers.vault().await.len(), 1);
}

/// Only a turn of a member's own agent is offered the connecting and taken
/// at its word: every other caller is nobody. An agent that anything narrows
/// is offered it, and taken, only where a rule of its names this very
/// action.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_turn_of_a_members_own_agent_is_offered_the_connecting() {
    let hers = Hers::new().await;
    hers.workspaces.call_private_model_addresses(true);
    let bo = admit(&hers.workspaces, &hers.workspace, &hers.owner, 2).await;
    // Another member's own agent's turn connects for the workspace too.
    let bos_caller = on_turn(&bo, &hers.workspace, SCOUT);
    let bos_turn = hers.turn(&bos_caller, None);
    assert_eq!(
        hers.connecting_offered(&bos_turn).await,
        [held::CONNECT],
        "another member's agent connects the workspace's calendars"
    );
    for (who, asks) in not_hosted_turns(&hers.ada, &hers.workspace) {
        let turn = hers.turn(&asks, None);
        not_a_turn(who, hers.tells(&turn, hers.telling()).await);
        assert!(hers.connecting_offered(&turn).await.is_empty(), "{who}");
    }

    let sub = on_turn(&hers.ada, &hers.workspace, "sub-agent");
    let named = |action: &str, answer: Answer| {
        naming(
            &hers.workspace,
            person_accounts::PROVIDER,
            held::RESOURCE,
            action,
            answer,
        )
    };
    // Every change of the provider's that can be undone: no outside effect.
    let whole_provider = Rule::new(
        TreePath::workspace(&hers.workspace).app(person_accounts::PROVIDER),
        KindSet::of(ActionKind::Undoable),
        Answer::Allow,
    );
    for (why, bound) in [
        (
            "a rule for another app alone",
            narrowed_elsewhere(&hers.workspace),
        ),
        (
            "a rule for the whole provider's changes",
            vec![whole_provider.clone()],
        ),
        (
            "a rule for a read",
            vec![named(held::ENTRIES, Answer::Allow)],
        ),
        (
            "a rule that denies it",
            vec![whole_provider, named(held::CONNECT, Answer::Deny)],
        ),
    ] {
        let turn = hers.turn(&sub, Some(&bound));
        let told = hers.tells(&turn, hers.telling()).await;
        assert!(
            matches!(told, Err(ActionError::NotPermitted)),
            "{why}: {told:?}"
        );
        assert!(hers.connecting_offered(&turn).await.is_empty(), "{why}");
    }
    let bound = vec![named(held::CONNECT, Answer::Allow)];
    let turn = hers.turn(&sub, Some(&bound));
    assert_eq!(hers.connecting_offered(&turn).await, [held::CONNECT]);
    hers.is_asked(&turn).await;
    assert!(
        hers.server.requests().is_empty(),
        "no calendar server was asked for any of them"
    );
}
