//! A person connects their calendar by telling their assistant where it is, and no device of
//! theirs is on while their server works.
//!
//! The whole way through the server's own interfaces, against a calendar server the test starts.
//! She says in her own words where her calendar is and under which user name. Her assistant's one
//! tool call hands the server that address, the user name and a name for the account, and nothing
//! else: no password, no reference, nothing that makes the account writable. The server asks her
//! for the password itself, on her own device in the system's protected field. What she types goes
//! from that field to the workspace's vault. The server then signs in to her calendar server, keeps
//! the account in the workspace for reading only and tells the assistant which calendars the server
//! lists, by their names. Her assistant reads an entry through its tools afterwards. The calendar
//! server was asked to read and nothing else. The password is in no request a model was sent, no
//! message, no log line and no file of the server in the clear, and on this way the model never
//! holds even the item's reference.
//!
//! What a telling is refused for asks nobody and connects nothing; a calendar server that refuses
//! the password is said so in its own words, and the item she typed is not kept.
use super::*;
use crate::workspace_actions::hosted_agent_turns::{SCOUT, conversation_part};
use crate::workspace_actions::hosted_secret_use::{
    Devices, Hosted, Lines, does, files_holding, logged,
};
use tilo_server::workspaces::person_model::scripted::Reply;
use tilo_server_protocol::certify::{ConfirmationAnswer, TypedSecret};

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

/// She says where her calendar is, and her assistant makes its one tool call: the question her
/// device is asked for the password, in the server's words.
async fn she_tells_and_is_asked(hosted: &Hosted, devices: &Devices, address: &str) -> String {
    hosted
        .turn(
            &format!("My calendar is at {address}, my user name there is {ADA}. Connect it."),
            vec![
                tells("call-1", address),
                Reply::text("I asked you for the password. Type it where your device asks."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    assert!(
        told.contains("protected field"),
        "her assistant is told that she is asked, in a field it cannot read: {told}"
    );
    let asked = devices.asked(1).await;
    assert_eq!(asked.len(), 1, "the telling asks for one password: {told}");
    let ask = asked[0].ask.as_ref().expect("a question in words");
    assert!(ask.secret && ask.choices.is_empty(), "{ask:?}");
    assert_eq!(asked[0].agent_id, SCOUT, "who asks is her assistant");
    ask.question.clone()
}

/// She types `typed` into the protected field her device shows.
fn she_types(devices: &Devices, typed: &str) {
    devices.answer(
        0,
        ConfirmationAnswer::Secret(TypedSecret::new(typed.to_owned())),
    );
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
    let devices = hosted.her_devices();

    // She tells. Her device asks her for the password, in the server's words, and nothing asks
    // her to allow a use. Her calendar server is asked nothing before she typed.
    let words = she_tells_and_is_asked(&hosted, &devices, &address).await;
    assert!(
        words.contains("reading only") && words.contains("Scout") && words.contains(ADA),
        "the question says what her typing does: {words}"
    );
    assert!(
        hosted.questions(chat::question::ACTION).await.is_empty()
            && hosted.questions(chat::question::CHOICE).await.is_empty(),
        "nothing asks in her conversation"
    );
    assert!(
        calendars.requests().is_empty(),
        "her calendar server is asked nothing before she typed"
    );
    assert!(her_accounts(&mut hosted).await.is_empty(), "nothing yet");

    // She types. Her server keeps it in the workspace's vault for that server alone, signs in,
    // keeps the account in the workspace for reading only. Her assistant is told once: the
    // calendars by their names, and nothing else of what her calendar server answered.
    she_types(&devices, TYPED);
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
    let vault = hosted.workspace_vault().await;
    assert_eq!(
        vault[0]["belongsTo"],
        serde_json::json!([place_of(&address)]),
        "{vault:?}"
    );
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
    assert_eq!(devices.count(), 1, "nobody is asked again");
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
    let devices = hosted.her_devices();

    she_tells_and_is_asked(&hosted, &devices, &address).await;
    she_types(&devices, MISTYPED);
    let told = the_assistant_is_told(
        &hosted,
        "Your calendar server did not take that password. Tell me again when you want to retry.",
    )
    .await;
    assert!(
        told.contains("rejected") && !told.contains("Family"),
        "her assistant is told the refusal in her calendar server's own words, and no \
         calendar: {told}"
    );
    assert!(
        hosted.workspace_vault().await.is_empty(),
        "the item she typed is not kept"
    );
    assert!(
        her_accounts(&mut hosted).await.is_empty(),
        "no account is kept"
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
    let devices = hosted.her_devices();
    let words = she_tells_and_is_asked(&hosted, &devices, "ada@calendar.example.test").await;
    assert!(
        words.contains("https://calendar.example.test"),
        "the question names the server her address belongs to: {words}"
    );
    assert!(her_accounts(&mut hosted).await.is_empty());
    let kept = hosted.workspace_vault().await;
    assert!(kept.is_empty(), "the workspace's vault keeps nothing more: {kept:?}");
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_telling_is_refused_for_asks_nobody_and_connects_nothing() {
    let mut hosted = Hosted::start().await;
    let devices = hosted.her_devices();
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
        assert_eq!(devices.count(), 0, "{what}: nobody is asked for a password");
    }
    assert!(her_accounts(&mut hosted).await.is_empty());
    let kept = hosted.workspace_vault().await;
    assert!(kept.is_empty(), "the workspace's vault keeps nothing more: {kept:?}");
    hosted.page.peer.close();
}
