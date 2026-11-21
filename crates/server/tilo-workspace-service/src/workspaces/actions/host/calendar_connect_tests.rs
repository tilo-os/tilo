//! A calendar connected by a member's agent telling the server where it is.
//! The turn of a member's agent hands over the address, the user name and a
//! name, and nothing else: no argument is a password, and none makes the
//! account writable. The member is asked for the password, and no calendar
//! server is asked anything before they typed. What they type connects the
//! account for reading only, once, for the whole workspace. A calendar
//! server that refuses the sign-in keeps nothing here, and what they typed
//! for it is not kept, while a login they kept before stays theirs. An
//! address that is not https or is inside is refused before anybody is
//! asked. Told twice it adds nothing. And no turn that is not a member's own
//! agent's is offered it.

use super::super::super::vault_tests::ask as of_her_vault;
use super::*;
use crate::workspaces::hosted_use::{ConnectAsk, Connected, TurnConnect};

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

    /// What `turn` is answered when it tells of the calendar `argument`
    /// names.
    async fn tells(
        &self,
        turn: &HostedTurn<'_>,
        argument: Value,
    ) -> Result<TurnConnect, ActionError> {
        let Value::Object(argument) = argument else {
            panic!("arguments are an object");
        };
        let request = InvokeRequest {
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
        };
        self.workspaces
            .connect_calendar_for_turn(turn, &request, Value::Null)
            .await
    }

    /// `turn` tells of her calendar, and she is asked for the password:
    /// what her server keeps of it.
    async fn is_asked(&self, turn: &HostedTurn<'_>) -> ConnectAsk {
        match self.tells(turn, self.telling()).await {
            Ok(TurnConnect::Asks(ask)) => *ask,
            other => panic!("her telling asks her for the password: {other:?}"),
        }
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

    /// She types `password` into the protected field under the question
    /// about `ask`: her device keeps it in her vault as the question says,
    /// and answers with its reference.
    async fn types(&self, ask: &ConnectAsk, password: &str) -> String {
        let login = ask.login();
        let keeping = json!({
            "kind": "login",
            "belongsTo": login.belongs_to,
            "username": login.username,
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

    /// What comes of her answer to the question about `ask`, read by the
    /// one reader that claims it: `typed` is the reference her device
    /// answered with, none where she declined.
    async fn answered(
        &self,
        turn: &HostedTurn<'_>,
        ask: &ConnectAsk,
        typed: Option<&str>,
    ) -> Result<Option<Connected>, String> {
        let claimed = self
            .workspaces
            .claim_connect(&ask.ask)
            .expect("her answer is claimed");
        self.workspaces
            .carry_out_connect(turn, &claimed, typed)
            .await
    }
}

/// The words `answer` refuses a telling in.
fn in_words(what: &str, answer: Result<TurnConnect, ActionError>) -> String {
    match answer {
        Err(ActionError::Refused { reason }) => reason,
        other => panic!("{what}: {other:?}"),
    }
}

/// A member's agent's turn tells of their calendar: they are asked for a
/// login at their calendar server and nobody else is asked anything. What
/// they type connects the account for reading only, every calendar of it
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
    let ask = hers.is_asked(&turn).await;
    let login = ask.login();
    assert_eq!(
        (login.belongs_to, login.username.as_str()),
        (vec![hers.place()], ADDRESS),
        "she is asked for a login at her calendar server, under her user name"
    );
    let words = ask.words();
    assert!(
        words.contains("reading only") && words.contains("Scout") && words.contains(ADDRESS),
        "the question says what her typing does: {words}"
    );
    assert!(
        hers.server.requests().is_empty(),
        "her calendar server is asked nothing before she typed"
    );
    assert!(hers.accounts().await.is_empty(), "and nothing is kept yet");

    let typed = hers.types(&ask, PASSWORD).await;
    let claimed = hers
        .workspaces
        .claim_connect(&ask.ask)
        .expect("her answer is claimed");
    assert!(
        hers.workspaces.claim_connect(&ask.ask).is_err(),
        "by one reader alone"
    );
    let connected = hers
        .workspaces
        .carry_out_connect(&turn, &claimed, Some(&typed))
        .await
        .expect("her calendar server takes the sign-in")
        .expect("an account");
    let account = format!(
        "caldav:{ADDRESS}@{}",
        hers.place().trim_start_matches("http://")
    );
    assert_eq!(
        serde_json::to_value(&connected).unwrap(),
        json!({"account": account, "name": NAME, "calendars": ["Family", "Work"]}),
        "the turn is told the calendars by their names, and nothing else"
    );
    assert!(
        hers.workspaces.claim_connect(&ask.ask).is_err(),
        "the ask is spent"
    );
    assert!(hers.workspaces.open_connects(&hers.workspace).is_empty());

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
        !format!("{ask:?} {connected:?} {accounts:?}").contains(PASSWORD),
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
    assert!(hers.workspaces.open_connects(&hers.workspace).is_empty());
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
    assert!(hers.workspaces.open_connects(&hers.workspace).is_empty());
    assert!(
        hers.server.requests().is_empty(),
        "no calendar server is asked anything"
    );
}

/// A calendar server that refuses the sign-in is said so in its own words.
/// Nothing is kept: no account, nothing to read, and the item they typed for
/// the question is taken out of their vault again. A login they kept for
/// that server before they told of it is theirs and stays, whatever comes of
/// it.
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
    let ask = hers.is_asked(&turn).await;
    let why = hers
        .answered(&turn, &ask, Some(&before))
        .await
        .expect_err("her calendar server refuses it");
    assert!(
        why.contains("rejected"),
        "in her calendar server's words: {why}"
    );
    assert_eq!(
        hers.vault().await.len(),
        1,
        "what she kept before the question stays hers"
    );

    // What she types for the question is not kept.
    let ask = hers.is_asked(&turn).await;
    let typed = hers.types(&ask, MISTYPED).await;
    assert_eq!(hers.vault().await.len(), 2);
    let why = hers
        .answered(&turn, &ask, Some(&typed))
        .await
        .expect_err("her calendar server refuses it");
    assert!(why.contains("rejected"), "{why}");
    assert_eq!(
        hers.vault().await.len(),
        1,
        "the item she typed for the question is taken out again"
    );
    assert!(hers.accounts().await.is_empty(), "no account is kept");
    assert!(hers.offered(&turn).await.is_empty(), "nothing to read");
    assert!(hers.workspaces.open_connects(&hers.workspace).is_empty());
    assert!(
        hers.server.mailed().is_empty(),
        "her calendar server wrote to nobody"
    );
}

/// Where she declines, nothing is connected and her calendar server is asked
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declining_connects_nothing() {
    let hers = Hers::new().await;
    hers.workspaces.call_private_model_addresses(true);
    let scout = on_turn(&hers.ada, &hers.workspace, SCOUT);
    let turn = hers.turn(&scout, None);

    let ask = hers.is_asked(&turn).await;
    let came = hers.answered(&turn, &ask, None).await;
    assert_eq!(came, Ok(None), "she declined");
    assert!(hers.accounts().await.is_empty());
    assert!(hers.workspaces.open_connects(&hers.workspace).is_empty());
    assert!(hers.server.requests().is_empty());
}

/// Told of the same account while she has not answered, the one question
/// stands. Told of it once it is connected, nothing is added and nobody is
/// asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_account_told_twice_asks_once_and_adds_nothing_once_connected() {
    let hers = Hers::new().await;
    hers.workspaces.call_private_model_addresses(true);
    let scout = on_turn(&hers.ada, &hers.workspace, SCOUT);
    let turn = hers.turn(&scout, None);

    let ask = hers.is_asked(&turn).await;
    let again = hers.tells(&turn, hers.telling()).await;
    assert_eq!(again, Ok(TurnConnect::Waits), "the one question stands");
    assert_eq!(hers.workspaces.open_connects(&hers.workspace).len(), 1);

    let typed = hers.types(&ask, PASSWORD).await;
    hers.answered(&turn, &ask, Some(&typed))
        .await
        .expect("her calendar server takes the sign-in")
        .expect("an account");
    let why = in_words(
        "told of it once more",
        hers.tells(&turn, hers.telling()).await,
    );
    assert!(why.contains("connected already"), "{why}");
    assert_eq!(hers.accounts().await.len(), 1);
    assert!(hers.workspaces.open_connects(&hers.workspace).is_empty());
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
        let told = hers.tells(&turn, hers.telling()).await;
        assert!(
            matches!(told, Err(ActionError::UnknownAction)),
            "{who}: {told:?}"
        );
        assert!(hers.connecting_offered(&turn).await.is_empty(), "{who}");
    }

    let sub = on_turn(&hers.ada, &hers.workspace, "sub-agent");
    let outside = KindSet::of(ActionKind::Outside);
    let named = |action: &str, answer: Answer| {
        tilo_automation::action_rule(
            person_accounts::PROVIDER,
            held::RESOURCE,
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
