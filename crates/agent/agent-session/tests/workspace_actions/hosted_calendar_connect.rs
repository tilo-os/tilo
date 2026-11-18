//! A person connects their calendar by telling their assistant where it is, and no device of
//! theirs is on while their server works.
//!
//! The whole way through the server's own interfaces, against a calendar server the test starts.
//! She says in her own words where her calendar is and under which user name. Her assistant's one
//! tool call hands the server that address, the user name and a name for the account, and nothing
//! else: no password, no reference, nothing that makes the account writable. The server asks her
//! for the password itself, in the protected field of her conversation. What she types goes from
//! her device to her vault. The server then signs in to her calendar server, keeps the account
//! for reading only, names her assistant for it and tells the assistant which calendars the
//! server lists, by their names. Her assistant reads an entry through its tools afterwards. The
//! calendar server was asked to read and nothing else. The password is in no request a model was
//! sent, no message, no log line and no file of the server in the clear, and on this way the
//! model never holds even the item's reference.
//!
//! What a telling is refused for asks nobody and connects nothing; a calendar server that refuses
//! the password is said so in its own words, and the item she typed is not kept.
use super::*;
use crate::workspace_actions::hosted_agent_turns::{SCOUT, conversation_part};
use crate::workspace_actions::hosted_secret_use::{Hosted, Lines, does, files_holding, logged};
use tilo_actions::remote::person_vault;
use tilo_server::workspaces::person_model::scripted::Reply;

const CHAT: &str = "so.tilo.chat";
/// What she types into the protected field. Made up for this test, and the sign-in of nothing
/// but the calendar server the test starts.
const TYPED: &str = "only-her-calendar-server-may-read-this";
/// What she types when she mistypes it.
const MISTYPED: &str = "not-what-her-calendar-server-knows";
/// What she calls the account.
const ACCOUNT: &str = "My own server";
/// What the tool call that connects a calendar is called.
const CONNECT: &str = "accounts/calendars#connect";

/// Her calendar server: two calendars, and one meeting in the first.
fn her_calendar_server() -> calendar_server::Server {
    let server = calendar_server::Server::start(ADA, TYPED);
    server.add_calendar("work", "Work");
    server.add_calendar("family", "Family");
    server.put("work", "review.ics", &review(meeting_start()));
    server
}

/// The place a sign-in for the server at `address` belongs to: its origin.
fn place_of(address: &str) -> String {
    address.trim_end_matches("/dav/").to_owned()
}

/// Her assistant's tool call that connects the calendar at `address`.
fn tells(call: &str, address: &str) -> Reply {
    does(
        call,
        CONNECT,
        &serde_json::json!({"serverUrl": address, "username": ADA, "name": ACCOUNT}),
    )
}

/// She says where her calendar is, and her assistant makes its one tool call: the question
/// that asks her for the password, with its id.
async fn she_tells_and_is_asked(
    hosted: &Hosted,
    address: &str,
) -> (ui::tilo_resource::Id, chat::chat::Message) {
    hosted
        .turn(
            &format!("My calendar is at {address}, my user name there is {ADA}. Connect it."),
            vec![
                tells("call-1", address),
                Reply::text("I asked you for the password. Type it into the field in our chat."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    assert!(
        told.contains("protected field"),
        "her assistant is told that she is asked, in a field it cannot read: {told}"
    );
    let asked = hosted.questions(chat::question::SECRET).await;
    assert_eq!(asked.len(), 1, "the telling asks for one password: {told}");
    asked.into_iter().next().expect("the one question")
}

/// What she types goes from her device to her vault on the server, and her device answers the
/// question with the reference the vault gave: the protected path, as her Chat window walks it.
async fn she_types(
    hosted: &mut Hosted,
    question: &ui::tilo_resource::Id,
    address: &str,
    typed: &str,
) {
    let kept = hosted
        .on_device(
            (
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::ADD,
            ),
            serde_json::json!({
                "kind": "login", "belongsTo": [place_of(address)], "username": ADA, "secret": typed,
            }),
        )
        .await;
    let reference = kept["reference"].as_str().expect("a reference").to_owned();
    hosted
        .answers(
            question,
            chat::question::FILLED,
            serde_json::json!({"reference": reference}),
        )
        .await;
}

/// What came of her answer, once her server said so on the question: the outcome's word and
/// what she is told with it.
async fn what_came_of_it(hosted: &Hosted) -> (String, String) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let now = hosted.questions(chat::question::SECRET).await;
        let outcome = now[0].1.question.as_ref().unwrap().outcome.clone();
        if let Some(outcome) = outcome.as_ref().filter(|outcome| {
            [
                chat::question::DONE,
                chat::question::FAILED,
                chat::question::DROPPED,
            ]
            .contains(&outcome.outcome.0.as_str())
        }) {
            return (outcome.outcome.0.clone(), outcome.detail.clone());
        }
        assert!(
            Instant::now() < deadline,
            "her server never said what came of her answer: {outcome:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Her assistant is woken once with what came of it, and answers `says`: everything its model
/// was sent for that turn.
async fn the_assistant_is_told(hosted: &Hosted, says: &str) -> String {
    hosted.model.then(Reply::text(says));
    let deadline = tilo_test_env::hang_deadline();
    let turns = loop {
        let turns = hosted.agents.turns(&hosted.workspace).await.unwrap();
        if !turns.is_empty() {
            break turns;
        }
        assert!(
            Instant::now() < deadline,
            "her assistant was never told what came of it"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(turns.len(), 1, "it is told once: {turns:?}");
    tokio::time::timeout(
        Duration::from_secs(60),
        hosted.agents.answer(&hosted.workspace, &turns[0]),
    )
    .await
    .expect("the turn ends")
    .unwrap();
    format!(
        "{:?}",
        hosted.model.asked().last().expect("it was asked").request
    )
}

/// Her calendar accounts, as her own device lists them.
async fn her_accounts(hosted: &mut Hosted) -> Vec<serde_json::Value> {
    let listed = hosted
        .on_device(
            (person_accounts::PROVIDER, CALENDARS, "list"),
            serde_json::Value::Null,
        )
        .await;
    listed["accounts"]
        .as_array()
        .unwrap_or_else(|| panic!("her accounts: {listed}"))
        .clone()
}

/// The items her vault keeps, as her own device lists them.
async fn her_vault(hosted: &mut Hosted) -> Vec<serde_json::Value> {
    let kept = hosted
        .on_device(
            (
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::LIST,
            ),
            serde_json::Value::Null,
        )
        .await;
    kept["items"]
        .as_array()
        .unwrap_or_else(|| panic!("her vault: {kept}"))
        .clone()
}

/// `typed` reached her calendar server's sign-in and nothing else: no request a model was sent,
/// no message of the conversation, no log line and no file of the server in the clear.
async fn it_is_nowhere_a_model_or_a_reader_looks(hosted: &Hosted, lines: &Lines, typed: &str) {
    for call in hosted.model.asked() {
        let sent = format!("{:?}", call.request);
        assert!(!sent.contains(typed), "a model request holds the password");
        assert!(
            !sent.contains("vault.item:"),
            "on this way a model holds not even the item's reference: {sent}"
        );
    }
    let part = conversation_part(&hosted.workspaces, &hosted.workspace).await;
    let said = format!("{:?}", part.messages.iter().collect::<Vec<_>>());
    assert!(said.contains(ADA) && !said.contains(typed));
    let saved = hosted
        .workspaces
        .resource_document(&hosted.workspace, CHAT, chat::parts::part(0).as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !saved
            .windows(typed.len())
            .any(|window| window == typed.as_bytes())
    );
    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    assert!(
        logged.len() > 1_000,
        "the log was observed: {} bytes",
        logged.len()
    );
    assert!(!logged.contains(typed), "a log line holds the password");
    let holding = files_holding(hosted.state.path(), typed.as_bytes());
    assert!(
        holding.is_empty(),
        "in the clear on the server's disk: {holding:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_assistant_connects_the_calendar_its_person_tells_it_of_for_reading_only() {
    let lines = logged();
    let mut hosted = Hosted::start().await;
    // The test's calendar server answers on this machine. Its operator lets this server call its
    // own network: the one leave a web call with a key is made there by.
    hosted.workspaces.call_private_model_addresses(true);
    let calendars = her_calendar_server();
    let address = calendars.url();

    // She tells. One question asks her for the password, in the server's words, and nothing
    // asks her to allow a use. Her calendar server is asked nothing before she typed.
    let (question, held) = she_tells_and_is_asked(&hosted, &address).await;
    assert_eq!(held.author, MemberId::agent(SCOUT));
    let asks = held.question.as_ref().unwrap();
    assert_eq!(
        asks.person,
        MemberId::person(hosted.person()),
        "only its own person is asked"
    );
    assert_eq!(
        asks.kept(),
        Some(serde_json::json!({
            "kind": "login", "belongsTo": [place_of(&address)], "username": ADA,
        })),
        "the field is for a login at her calendar server, under her user name"
    );
    assert!(
        held.text.contains("reading only") && held.text.contains("Scout"),
        "the question says what her typing does: {}",
        held.text
    );
    assert!(
        hosted.questions(chat::question::ACTION).await.is_empty(),
        "one question, and none about a use"
    );
    assert!(
        calendars.requests().is_empty(),
        "her calendar server is asked nothing before she typed"
    );
    assert!(her_accounts(&mut hosted).await.is_empty(), "nothing yet");

    // She types. Her server signs in, keeps the account for reading only and names her
    // assistant for it.
    she_types(&mut hosted, &question, &address, TYPED).await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    let (came, told) = what_came_of_it(&hosted).await;
    assert_eq!(came, chat::question::DONE, "{told}");
    let accounts = her_accounts(&mut hosted).await;
    assert_eq!(accounts.len(), 1, "{accounts:?}");
    let account = &accounts[0];
    assert_eq!(account["name"], ACCOUNT, "{account}");
    assert_eq!(
        account["readOnly"], true,
        "connected for reading only: {account}"
    );
    let its_calendars = account["calendars"].as_array().expect("its calendars");
    assert_eq!(its_calendars.len(), 2, "{account}");
    assert!(
        its_calendars
            .iter()
            .all(|calendar| calendar["readOnly"] == true),
        "every calendar of it is one that is not written: {account}"
    );
    let id = account["id"].as_str().expect("its id").to_owned();
    let named = hosted
        .on_device(
            (
                person_accounts::PROVIDER,
                person_accounts::RESOURCE,
                person_accounts::ASSISTANTS,
            ),
            serde_json::json!({"account": id}),
        )
        .await;
    assert_eq!(named["agents"], serde_json::json!([SCOUT]), "{named}");

    // Her assistant is told once: the calendars by their names, and nothing else of what her
    // calendar server answered.
    let told = the_assistant_is_told(
        &hosted,
        "Your calendar is connected for reading only. It has Family and Work.",
    )
    .await;
    assert!(
        told.contains("Family") && told.contains("Work") && told.contains("reading only"),
        "{told}"
    );
    assert!(
        !told.contains("/dav/calendars/"),
        "no address of a calendar reaches the model: {told}"
    );

    // It reads an entry through its tools.
    let starts = meeting_start();
    hosted
        .turn(
            "What is in my calendar in the next hours?",
            vec![
                does(
                    "call-2",
                    "accounts/calendars#entries",
                    &serde_json::json!({
                        "from": with_offset(later(starts, -120)),
                        "to": with_offset(later(starts, 120)),
                    }),
                ),
                Reply::text("You have the Quarter review."),
            ],
        )
        .await;
    let read = hosted.last_tool_result();
    assert!(
        read.contains(TITLE),
        "its turn reads what she connected: {read}"
    );

    // Told of the same account again, it adds nothing and asks nobody.
    hosted
        .turn(
            "Connect that calendar once more.",
            vec![
                tells("call-3", &address),
                Reply::text("It is connected already."),
            ],
        )
        .await;
    let again = hosted.last_tool_result();
    assert!(again.contains("connected already"), "{again}");
    assert_eq!(hosted.questions(chat::question::SECRET).await.len(), 1);
    assert_eq!(her_accounts(&mut hosted).await.len(), 1);

    // Her calendar server was asked to read, and wrote to nobody.
    let methods: Vec<String> = calendars
        .requests()
        .into_iter()
        .map(|request| request.method)
        .collect();
    assert!(
        !methods.is_empty()
            && methods
                .iter()
                .all(|method| matches!(method.as_str(), "PROPFIND" | "REPORT" | "GET")),
        "her calendar server was asked for more than a read: {methods:?}"
    );
    assert_eq!(calendars.mailed(), Vec::<String>::new());

    it_is_nowhere_a_model_or_a_reader_looks(&hosted, &lines, TYPED).await;
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_calendar_server_that_refuses_the_password_is_said_so_and_no_item_is_kept() {
    let lines = logged();
    let mut hosted = Hosted::start().await;
    hosted.workspaces.call_private_model_addresses(true);
    let calendars = her_calendar_server();
    let address = calendars.url();

    let (question, _) = she_tells_and_is_asked(&hosted, &address).await;
    she_types(&mut hosted, &question, &address, MISTYPED).await;
    assert_eq!(her_vault(&mut hosted).await.len(), 1, "what she typed");
    hosted.agents.look(&hosted.workspace).await.unwrap();

    let (came, told) = what_came_of_it(&hosted).await;
    assert_eq!(came, chat::question::FAILED, "{told}");
    assert!(
        told.contains("rejected"),
        "she is told in her calendar server's own words: {told}"
    );
    assert!(
        her_vault(&mut hosted).await.is_empty(),
        "the item she typed is not kept"
    );
    assert!(
        her_accounts(&mut hosted).await.is_empty(),
        "no account is kept"
    );
    let told = the_assistant_is_told(
        &hosted,
        "Your calendar server did not take that password. Tell me again when you want to retry.",
    )
    .await;
    assert!(
        told.contains("rejected") && !told.contains("Family"),
        "her assistant is told the refusal and no calendar: {told}"
    );
    it_is_nowhere_a_model_or_a_reader_looks(&hosted, &lines, MISTYPED).await;
    hosted.page.peer.close();
}

/// She says no address, only who she is there: her mail address. The server takes it as she
/// says it, looks for her calendar under the name her address ends in, and asks her for the
/// password with a question that names that server before she types anything. Nothing is
/// connected and nothing is kept before she does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mail_address_alone_is_asked_for_at_the_server_of_its_name() {
    let mut hosted = Hosted::start().await;
    // This server's operator lets it call its own network, so no name is looked up here.
    hosted.workspaces.call_private_model_addresses(true);
    let (_, held) = she_tells_and_is_asked(&hosted, "ada@calendar.example.test").await;
    assert!(
        held.text.contains("https://calendar.example.test"),
        "the question names the server her address belongs to: {}",
        held.text
    );
    assert!(her_accounts(&mut hosted).await.is_empty());
    assert!(her_vault(&mut hosted).await.is_empty());
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_telling_is_refused_for_asks_nobody_and_connects_nothing() {
    let mut hosted = Hosted::start().await;
    // No leave here: this server calls public addresses alone, as a server does by default.
    let telling = |more: serde_json::Value| {
        let mut args = serde_json::json!({"username": ADA, "name": ACCOUNT});
        for (name, value) in more.as_object().expect("arguments").clone() {
            args[name.as_str()] = value;
        }
        args
    };
    for (what, args, words) in [
        (
            "an address that is not https",
            telling(serde_json::json!({"serverUrl": "http://calendar.example.test/dav/"})),
            "https",
        ),
        (
            "this machine itself",
            telling(serde_json::json!({"serverUrl": "https://127.0.0.1:8443/dav/"})),
            "public",
        ),
        (
            "an address of a private network",
            telling(serde_json::json!({"serverUrl": "https://10.0.0.8/dav/"})),
            "public",
        ),
        (
            "an address that carries a sign-in",
            telling(serde_json::json!({"serverUrl": "https://ada:made-up@calendar.example.test/"})),
            "sign-in",
        ),
        (
            "an argument that would make the account writable",
            telling(serde_json::json!({
                "serverUrl": "https://calendar.example.test/dav/", "readOnly": false,
            })),
            "names its server",
        ),
        (
            "a password in the call",
            telling(serde_json::json!({
                "serverUrl": "https://calendar.example.test/dav/", "password": "made-up",
            })),
            "names its server",
        ),
    ] {
        hosted
            .turn(
                "Connect my calendar.",
                vec![
                    does("call-1", CONNECT, &args),
                    Reply::text("I could not connect it."),
                ],
            )
            .await;
        let told = hosted.last_tool_result();
        assert!(told.contains(words), "{what}: {told}");
        assert!(
            hosted.questions(chat::question::SECRET).await.is_empty(),
            "{what}: nobody is asked for a password"
        );
    }
    assert!(her_accounts(&mut hosted).await.is_empty());
    assert!(her_vault(&mut hosted).await.is_empty());
    hosted.page.peer.close();
}
