//! A person signs in to their mail by telling their assistant its address, and no device of
//! theirs is on while their server works.
//!
//! The whole way through the server's own interfaces. She writes one sentence, "log in my mail
//! account" and its address. Her assistant's one tool call hands the server that address and
//! nothing else. The server works out the account's two servers, asks her for the password
//! itself, in the protected field of her conversation, with a question that names both servers,
//! signs in with what she typed, keeps the account, names her assistant for it and wakes the
//! assistant once. Nobody is asked to allow a use. The password is in no request a model was
//! sent, no message, no log line and no file of the server in the clear, and on this way the
//! model never holds even the item's reference.
//!
//! The network is stood in for: what works an address's servers out and what proves a sign-in
//! answer from this file. A password her mail server refuses is asked for again by the server
//! itself, and no ask of the model is spent on it.
use super::*;
use tilo_server::workspaces::hosted_use::{MailFound, MailServer, MailServers};

/// Her mail address, and the two servers her provider keeps it on.
const ADDRESS: &str = "ada@example.test";
const READS: &str = "imap.provider.test";
const SENDS: &str = "smtp.provider.test";
/// Where a sign-in for those two servers belongs, as her vault names it.
const PLACES: [&str; 2] = [
    "imap://imap.provider.test:993",
    "smtp://smtp.provider.test:465",
];
/// What she types into the protected field. Made up for this file, and the sign-in of nothing
/// but the stand-in below.
const TYPED: &str = "only-her-mail-servers-may-read-this";
/// What she types when she mistypes it.
const MISTYPED: &str = "not-what-her-mail-server-knows";
/// What the tool call that signs a mail account in is called.
const CONNECT: &str = "accounts/accounts#connect";
/// The sentence a person wrote to their assistant, with a made-up address in it.
const SENTENCE: &str = "Perfect thanks can you pls log in my mail account ada@example.test";

/// Where an address's mail lives, as the stand-in for the network works it out: her address
/// lives at the two servers her provider publishes, where this server may ask them; an address
/// at a provider that signs in on a page of its own says so; nothing is found for any other.
fn finds(address: &str, allowed: &dyn Fn(&str) -> bool) -> Result<MailServers, String> {
    let at = |reads: &str, sends: &str, own_sign_in| MailServers {
        incoming: MailServer::new(reads, 993),
        outgoing: MailServer::new(sends, 465),
        found: MailFound::Published,
        username: None,
        own_sign_in,
    };
    match address {
        ADDRESS if allowed(READS) && allowed(SENDS) => Ok(at(READS, SENDS, None)),
        "ada@bigmail.test" => Ok(at(
            "imap.bigmail.test",
            "smtp.bigmail.test",
            Some("Bigmail"),
        )),
        _ => Err(
            "No mail server was found for that address: say the name of its incoming (IMAP) \
             server"
                .to_owned(),
        ),
    }
}

/// A server whose stand-ins answer for her mail: a sign-in takes [`TYPED`] alone and answers
/// anything else as a mail server that refuses a password does. The accounts signed in.
async fn started() -> (Hosted, Arc<Mutex<Vec<String>>>) {
    let hosted = Hosted::start().await;
    let signed_in = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&signed_in);
    hosted
        .workspaces
        .sign_in_accounts_through(move |account, password| {
            if password == TYPED {
                kept.lock().unwrap().push(account.to_owned());
                Ok("signed in".into())
            } else {
                Err("IMAP server rejected the saved credential".into())
            }
        });
    hosted.workspaces.find_mail_servers_through(finds);
    (hosted, signed_in)
}

/// Her assistant's one tool call: the address, and nothing else.
fn tells(call: &str) -> Reply {
    does(call, CONNECT, &serde_json::json!({"address": ADDRESS}))
}

/// She writes the sentence, and her assistant makes its one tool call: the question that asks
/// her for the password, with its id.
async fn she_says_it_and_is_asked(hosted: &Hosted) -> (Id, chat::chat::Message) {
    hosted
        .turn(
            SENTENCE,
            vec![
                tells("call-1"),
                Reply::text("I asked you for its password. Type it into the field in our chat."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    assert!(
        told.contains("protected field") && told.contains(READS) && told.contains(SENDS),
        "her assistant is told that she is asked, in a field it cannot read, and which servers \
         were found: {told}"
    );
    let asked = hosted.questions(chat::question::SECRET).await;
    assert_eq!(asked.len(), 1, "the telling asks for one password: {told}");
    asked.into_iter().next().expect("the one question")
}

/// What she types goes from her device to her vault on the server, bound to her two mail
/// servers, and her device answers the question with the reference the vault gave: the
/// protected path, as her Chat window walks it.
async fn she_types(hosted: &mut Hosted, question: &Id, typed: &str) {
    let kept = hosted
        .on_device(
            (
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::ADD,
            ),
            serde_json::json!({
                "kind": "login", "belongsTo": PLACES, "username": ADDRESS, "secret": typed,
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

/// The questions that ask for a password, once there are `many` of them.
async fn asked_for_a_password(hosted: &Hosted, many: usize) -> Vec<(Id, chat::chat::Message)> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let asked = hosted.questions(chat::question::SECRET).await;
        if asked.len() >= many {
            return asked;
        }
        assert!(
            Instant::now() < deadline,
            "her server asked for a password {} times, not {many}",
            asked.len()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// What came of her answer to the question at `place` among those that ask for a password,
/// once her server said so on it: the outcome's word and what she is told with it.
async fn what_came_of(hosted: &Hosted, place: usize) -> (String, String) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let now = hosted.questions(chat::question::SECRET).await;
        let outcome = now[place].1.question.as_ref().unwrap().outcome.clone();
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

/// Her mail accounts, as her own device lists them.
async fn her_mail(hosted: &mut Hosted) -> Vec<serde_json::Value> {
    let listed = hosted
        .on_device(
            (
                person_accounts::PROVIDER,
                person_accounts::RESOURCE,
                person_accounts::LIST,
            ),
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

/// `typed` reached the sign-in and nothing else: no request a model was sent, no message of
/// the conversation, no log line and no file of the server in the clear.
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
    assert!(said.contains(ADDRESS) && !said.contains(typed));
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
async fn the_assistant_signs_a_mail_account_in_from_its_address_alone() {
    let lines = logged();
    let (mut hosted, signed_in) = started().await;
    // The stand-in's servers answer under no name of the network. Its operator lets this server
    // ask what it would not find there: the one leave a web call with a key is made by.
    hosted.workspaces.call_private_model_addresses(true);

    // She says it. One question asks her for the password, in the server's words, which name
    // both servers; nothing asks her to allow a use; nothing is signed in before she typed.
    let (question, held) = she_says_it_and_is_asked(&hosted).await;
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
            "kind": "login", "belongsTo": PLACES, "username": ADDRESS,
        })),
        "the field is for a login at her two mail servers, under her address"
    );
    assert!(
        [ADDRESS, READS, SENDS, "Scout", "I never see it"]
            .iter()
            .all(|part| held.text.contains(part)),
        "the question names her account, both servers and who reads there: {}",
        held.text
    );
    assert!(
        hosted.questions(chat::question::ACTION).await.is_empty(),
        "one question, and none about a use"
    );
    assert!(
        signed_in.lock().unwrap().is_empty(),
        "nothing is signed in before she typed"
    );
    assert!(her_mail(&mut hosted).await.is_empty(), "nothing yet");

    // She types. Her server signs in, keeps the account and names her assistant for it.
    she_types(&mut hosted, &question, TYPED).await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    let (came, told) = what_came_of(&hosted, 0).await;
    assert_eq!(came, chat::question::DONE, "{told}");
    assert!(told.contains(READS) && told.contains(SENDS), "{told}");
    assert_eq!(
        signed_in.lock().unwrap().clone(),
        vec![ADDRESS.to_owned()],
        "her account is signed in with what her vault keeps, once"
    );
    let accounts = her_mail(&mut hosted).await;
    assert_eq!(accounts.len(), 1, "{accounts:?}");
    let account = &accounts[0];
    assert_eq!(account["id"], ADDRESS, "{account}");
    assert_eq!(account["hasSecret"], true, "{account}");
    let kept = account.to_string();
    assert!(
        kept.contains(READS) && kept.contains(SENDS),
        "the account is kept at the servers that were found: {kept}"
    );
    let named = hosted
        .on_device(
            (
                person_accounts::PROVIDER,
                person_accounts::RESOURCE,
                person_accounts::ASSISTANTS,
            ),
            serde_json::json!({"account": ADDRESS}),
        )
        .await;
    assert_eq!(named["agents"], serde_json::json!([SCOUT]), "{named}");

    // Her assistant is told once, and says so in one line.
    let told = the_assistant_is_told(&hosted, "Your mail account is signed in.").await;
    assert!(
        told.contains("signed in") && told.contains(READS) && told.contains(SENDS),
        "{told}"
    );
    // The sentence took three asks of the model: the call, its closing words, and the news.
    assert_eq!(
        hosted.model.asked().len(),
        3,
        "asks of the model from her sentence to the account signed in"
    );

    // Told of the same account again, it adds nothing and asks nobody.
    hosted
        .turn(
            "Log in my mail account ada@example.test once more.",
            vec![tells("call-2"), Reply::text("It is signed in already.")],
        )
        .await;
    let again = hosted.last_tool_result();
    assert!(again.contains("signed in already"), "{again}");
    assert_eq!(hosted.questions(chat::question::SECRET).await.len(), 1);
    assert_eq!(her_mail(&mut hosted).await.len(), 1);

    it_is_nowhere_a_model_or_a_reader_looks(&hosted, &lines, TYPED).await;
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_password_her_mail_server_refuses_is_asked_for_again_by_the_server_itself() {
    let lines = logged();
    let (mut hosted, signed_in) = started().await;
    hosted.workspaces.call_private_model_addresses(true);

    let (first, _) = she_says_it_and_is_asked(&hosted).await;
    let asked_of_the_model = hosted.model.asked().len();
    she_types(&mut hosted, &first, MISTYPED).await;
    assert_eq!(her_vault(&mut hosted).await.len(), 1, "what she typed");
    hosted.agents.look(&hosted.workspace).await.unwrap();

    // The first question says which server did not take it, and a second one asks again: the
    // same field, waiting, saying what was refused.
    let (came, told) = what_came_of(&hosted, 0).await;
    assert_eq!(came, chat::question::FAILED, "{told}");
    assert!(
        told.contains("not accepted") && told.contains(READS),
        "she is told which server refused it: {told}"
    );
    let asked = asked_for_a_password(&hosted, 2).await;
    assert_eq!(asked.len(), 2, "she is asked once more, and once only");
    let (second, again) = &asked[1];
    let asks = again.question.as_ref().unwrap();
    assert!(asks.outcome.is_none(), "the second question waits for her");
    assert_eq!(
        asks.kept(),
        Some(serde_json::json!({
            "kind": "login", "belongsTo": PLACES, "username": ADDRESS,
        })),
        "the same field, for the same two servers"
    );
    assert!(
        again.text.contains("not accepted") && again.text.contains(READS),
        "the second question says what was refused: {}",
        again.text
    );
    assert!(
        her_vault(&mut hosted).await.is_empty(),
        "the item she mistyped is not kept"
    );
    assert!(her_mail(&mut hosted).await.is_empty(), "no account is kept");
    assert!(signed_in.lock().unwrap().is_empty());

    // No model is asked anything for a mistyped password, and her assistant is not woken.
    hosted.agents.look(&hosted.workspace).await.unwrap();
    assert!(
        hosted
            .agents
            .turns(&hosted.workspace)
            .await
            .unwrap()
            .is_empty(),
        "her assistant is not woken for a mistyped password"
    );
    assert_eq!(
        hosted.model.asked().len(),
        asked_of_the_model,
        "no ask of the model is spent on it"
    );

    // She types it again under the second question, and her account is signed in.
    she_types(&mut hosted, second, TYPED).await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    let (came, told) = what_came_of(&hosted, 1).await;
    assert_eq!(came, chat::question::DONE, "{told}");
    assert_eq!(signed_in.lock().unwrap().clone(), vec![ADDRESS.to_owned()]);
    assert_eq!(her_mail(&mut hosted).await.len(), 1);
    assert_eq!(her_vault(&mut hosted).await.len(), 1, "what she typed last");
    the_assistant_is_told(&hosted, "Your mail account is signed in.").await;
    assert_eq!(
        hosted.model.asked().len(),
        3,
        "three asks of the model in all, a mistyped password among her answers"
    );

    it_is_nowhere_a_model_or_a_reader_looks(&hosted, &lines, MISTYPED).await;
    it_is_nowhere_a_model_or_a_reader_looks(&hosted, &lines, TYPED).await;
    hosted.page.peer.close();
}

/// The address of a person whose own mail server answers at ports its domain publishes
/// nowhere, and that server as she names it: its name, and each port after a colon.
const OWN_ADDRESS: &str = "ada@nowhere.test";
const OWN_SERVER: &str = "mail.nowhere.test";
const OWN_READS: &str = "mail.nowhere.test:1993";
const OWN_SENDS: &str = "mail.nowhere.test:1587";
/// Where a sign-in for that server belongs, as her vault names it.
const OWN_PLACES: [&str; 2] = [
    "imap://mail.nowhere.test:1993",
    "smtp://mail.nowhere.test:1587",
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_named_with_its_port_is_signed_in_to_at_that_port() {
    let (mut hosted, signed_in) = started().await;
    // Her own server answers under no name of the network: the operator's one leave, as above.
    hosted.workspaces.call_private_model_addresses(true);

    // Nothing is found for her address, so her assistant passes on the two servers as she said
    // them, each with its port. One question asks her for the password, for a login at exactly
    // those two places.
    hosted
        .turn(
            "Log in my mail account ada@nowhere.test. Its mail is read at mail.nowhere.test:1993 \
             and sent through mail.nowhere.test:1587.",
            vec![
                does(
                    "call-1",
                    CONNECT,
                    &serde_json::json!({
                        "address": OWN_ADDRESS,
                        "incomingServer": OWN_READS,
                        "outgoingServer": OWN_SENDS,
                    }),
                ),
                Reply::text("I asked you for its password. Type it into the field in our chat."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    let asked = hosted.questions(chat::question::SECRET).await;
    assert_eq!(
        asked.len(),
        1,
        "a server named with its port is taken, and its person asked once: {told}"
    );
    assert!(told.contains(OWN_SERVER), "{told}");
    let (question, held) = asked.into_iter().next().expect("the one question");
    assert_eq!(
        held.question.as_ref().unwrap().kept(),
        Some(serde_json::json!({
            "kind": "login", "belongsTo": OWN_PLACES, "username": OWN_ADDRESS,
        })),
        "the field is for a login at her server's two ports"
    );

    // She types. Her server signs in and keeps the account at the ports she named.
    let kept = hosted
        .on_device(
            (
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::ADD,
            ),
            serde_json::json!({
                "kind": "login", "belongsTo": OWN_PLACES, "username": OWN_ADDRESS, "secret": TYPED,
            }),
        )
        .await;
    let reference = kept["reference"].as_str().expect("a reference").to_owned();
    hosted
        .answers(
            &question,
            chat::question::FILLED,
            serde_json::json!({"reference": reference}),
        )
        .await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    let (came, said) = what_came_of(&hosted, 0).await;
    assert_eq!(came, chat::question::DONE, "{said}");
    assert_eq!(
        signed_in.lock().unwrap().clone(),
        vec![OWN_ADDRESS.to_owned()]
    );
    let accounts = her_mail(&mut hosted).await;
    assert_eq!(accounts.len(), 1, "{accounts:?}");
    let account = &accounts[0];
    let listed = account.to_string();
    assert!(
        listed.contains(OWN_SERVER),
        "the account is kept at the server she named: {listed}"
    );
    assert_eq!(account["incomingPort"], 1993, "{account}");
    assert_eq!(account["outgoingPort"], 1587, "{account}");
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_telling_is_refused_for_asks_nobody_for_a_password() {
    let (mut hosted, signed_in) = started().await;
    // No leave here: this server asks public servers alone, as a server does by default.
    for (what, args, words) in [
        (
            "a provider that signs in on a page of its own",
            serde_json::json!({"address": "ada@bigmail.test"}),
            "page of its own",
        ),
        (
            "an address no server is found for",
            serde_json::json!({"address": "ada@nowhere.test"}),
            "incomingServer",
        ),
        (
            "servers named on this machine itself",
            serde_json::json!({
                "address": "ada@nowhere.test",
                "incomingServer": "localhost",
                "outgoingServer": "localhost",
            }),
            "public internet",
        ),
        (
            "a server named with what is no port",
            serde_json::json!({
                "address": "ada@nowhere.test",
                "incomingServer": "mail.nowhere.test:imap",
                "outgoingServer": "mail.nowhere.test",
            }),
            "after a colon",
        ),
        (
            "a password in the call",
            serde_json::json!({"address": ADDRESS, "password": "made-up"}),
            "nothing else",
        ),
        (
            "a vault item in the call",
            serde_json::json!({"address": ADDRESS, "item": "vault.item:made-up"}),
            "nothing else",
        ),
    ] {
        hosted
            .turn(
                "Log in my mail account.",
                vec![
                    does("call-1", CONNECT, &args),
                    Reply::text("I could not sign it in."),
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
    assert!(her_mail(&mut hosted).await.is_empty());
    assert!(her_vault(&mut hosted).await.is_empty());
    assert!(signed_in.lock().unwrap().is_empty());
    hosted.page.peer.close();
}
