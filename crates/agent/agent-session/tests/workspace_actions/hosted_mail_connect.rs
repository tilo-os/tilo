//! A person signs in to their mail by telling their assistant its address, and no device of
//! theirs is on while their server works.
//!
//! The whole way through the server's own interfaces. She writes one sentence, "log in my mail
//! account" and its address. Her assistant's one tool call hands the server that address and
//! nothing else. The server works out the account's two servers, asks her for the password itself,
//! on her own device in the system's protected field, with a question that names both servers,
//! keeps what she typed in the workspace's vault, signs in with it, keeps the account in the
//! workspace and wakes the assistant once. Nobody is asked to allow a use. The password is in no
//! request a model was sent, no message, no log line and no file of the server in the clear, and on
//! this way the model never holds even the item's reference.
//!
//! The network is stood in for: what works an address's servers out and what proves a sign-in
//! answer from this file. A password her mail server refuses is asked for again by the server
//! itself, and no ask of the model is spent on it. One test signs in at a mail server with real
//! sockets instead, reads her inbox there and sees a message she sends herself arrive.
use super::*;
use tilo_server::workspaces::hosted_use::{MailFound, MailServer, MailServers};

/// Her mail address, and the two servers her provider keeps it on.
const ADDRESS: &str = "ada@example.test";
const READS: &str = "imap.provider.test";
const SENDS: &str = "smtp.provider.test";
/// Where a sign-in for those two servers belongs, as the workspace's vault names it.
const PLACES: [&str; 2] = [
    "imap://imap.provider.test:993",
    "smtp://smtp.provider.test:465",
];
/// What she types into the protected field her device shows. Made up for this file, and the sign-in of nothing
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

/// She writes the sentence, and her assistant makes its one tool call: the record that waits
/// for her password, in the server's words, naming her account and both servers and the
/// vault's own add as what answers it.
async fn she_says_it_and_is_asked(hosted: &mut Hosted) -> tilo_actions::remote::asked::Asked {
    hosted
        .turn(
            SENTENCE,
            vec![
                tells("call-1"),
                Reply::text("I asked you for its password. Type it where your device asks."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    assert!(
        told.contains(ASKED) && told.contains(READS) && told.contains(SENDS),
        "her assistant is told that she is asked, and which servers were found: {told}"
    );
    let asked = hosted.page.asks().await;
    assert_eq!(asked.len(), 1, "the telling asks for one password: {told}");
    asked.into_iter().next().expect("the one question")
}

/// She keeps `typed` in the workspace's vault as the login for both servers, as the protected
/// field fills the vault's own add: its reference.
async fn she_types(hosted: &mut Hosted, typed: &str) -> String {
    let kept = hosted
        .on_device(
            (
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::ADD,
            ),
            serde_json::json!({
                "kind": "login",
                "belongsTo": PLACES,
                "username": ADDRESS,
                "secret": typed,
            }),
        )
        .await;
    kept["reference"]
        .as_str()
        .unwrap_or_else(|| panic!("the vault keeps it: {kept}"))
        .to_owned()
}

/// The person answers the one question waiting for them with `resolution`.
async fn approves(hosted: &mut Hosted, resolution: &str) {
    let asked = hosted.page.asks().await;
    assert_eq!(asked.len(), 1, "one question waits: {asked:?}");
    hosted
        .page
        .resolve(&asked[0], resolution, None)
        .await
        .unwrap_or_else(|why| panic!("the person answers: {why}"));
}

/// What her device is shown of the question: a password, in the server's words, which name her
/// account and its servers; the vault's add is what answers it.
fn asks_for_her_password(asked: &tilo_actions::remote::asked::Asked, servers: &[&str]) -> String {
    use tilo_actions::remote::asked::Fact;
    assert_eq!(
        asked.facts.takes.as_deref(),
        Some("tilo.vault/vault#add"),
        "the vault's own add answers it: {asked:?}"
    );
    assert_eq!(asked.facts.verb, "sign in", "{asked:?}");
    assert!(
        asked.facts.facts.contains(&Fact::new("kind", "login")),
        "{asked:?}"
    );
    assert!(
        servers.iter().all(|part| asked.facts.words.contains(part)),
        "the question names her account and both servers: {}",
        asked.facts.words
    );
    asked.facts.words.clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_assistant_signs_a_mail_account_in_from_its_address_alone() {
    let lines = logged();
    let (mut hosted, signed_in) = started().await;
    // The stand-in's servers answer under no name of the network. Its operator lets this server
    // ask what it would not find there: the one leave a web call with a key is made by.
    hosted.workspaces.call_private_model_addresses(true);

    // She says it. Her device asks her for the password, in the server's words, which name
    // both servers; nothing asks her to allow a use; nothing is signed in before she typed.
    let asked = she_says_it_and_is_asked(&mut hosted).await;
    asks_for_her_password(&asked, &[ADDRESS, READS, SENDS, "Scout"]);
    assert!(
        matches!(
            &asked.facts.by,
            tilo_actions::remote::asked::AskedBy::Agent(who) if who.id == SCOUT
        ),
        "who asks is her assistant: {asked:?}"
    );
    assert!(
        signed_in.lock().unwrap().is_empty(),
        "nothing is signed in before she typed"
    );
    assert!(her_mail(&mut hosted).await.is_empty(), "nothing yet");

    // She types. Her server keeps it in the workspace's vault, signs in, keeps the account in the
    // workspace, and her assistant is told once and says so in one line.
    let _ = she_types(&mut hosted, TYPED).await;
    hosted
        .model
        .then(Reply::text("Your mail account is signed in."));
    approves(&mut hosted, "approve").await;
    let told = super::resumed(&mut hosted, "signed in").await;
    assert!(
        told.contains("signed in") && told.contains(READS) && told.contains(SENDS),
        "{told}"
    );
    assert_eq!(
        signed_in.lock().unwrap().clone(),
        vec![ADDRESS.to_owned()],
        "her account is signed in with what the workspace's vault keeps, once"
    );
    let vault = hosted.workspace_vault().await;
    assert_eq!(vault.len(), 1, "{vault:?}");
    assert_eq!(
        vault[0]["belongsTo"],
        serde_json::json!(PLACES),
        "what she typed is kept for her two mail servers alone"
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
    assert!(hosted.page.asks().await.is_empty(), "nobody is asked again");
    assert_eq!(her_mail(&mut hosted).await.len(), 1);

    it_is_nowhere_a_model_or_a_reader_looks(&hosted, &lines, TYPED).await;
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_password_her_mail_server_refuses_signs_nothing_in() {
    let lines = logged();
    let (mut hosted, signed_in) = started().await;
    hosted.workspaces.call_private_model_addresses(true);

    she_says_it_and_is_asked(&mut hosted).await;
    let _ = she_types(&mut hosted, MISTYPED).await;
    hosted.model.then(Reply::text("It did not sign in."));
    approves(&mut hosted, "approve").await;
    let told = super::resumed(&mut hosted, "rejected").await;
    assert!(told.contains("rejected"), "{told}");

    // The vault keeps what she typed, and her mail server took none of it: no account is kept
    // and nothing waits for her.
    assert_eq!(hosted.workspace_vault().await.len(), 1, "what she typed");
    assert!(her_mail(&mut hosted).await.is_empty(), "no account is kept");
    assert!(signed_in.lock().unwrap().is_empty());
    assert!(hosted.page.asks().await.is_empty(), "nothing waits");

    // She keeps the right password, and the telling is asked again and signs in.
    let _ = she_types(&mut hosted, TYPED).await;
    let asked = {
        hosted
            .turn(
                "Log in my mail account ada@example.test.",
                vec![tells("call-2"), Reply::text("I asked you again.")],
            )
            .await;
        hosted.page.asks().await
    };
    assert_eq!(asked.len(), 1, "the telling asks again");
    hosted
        .model
        .then(Reply::text("Your mail account is signed in."));
    approves(&mut hosted, "approve").await;
    super::resumed(&mut hosted, "signed in").await;
    assert_eq!(signed_in.lock().unwrap().clone(), vec![ADDRESS.to_owned()]);
    assert_eq!(her_mail(&mut hosted).await.len(), 1);

    it_is_nowhere_a_model_or_a_reader_looks(&hosted, &lines, MISTYPED).await;
    it_is_nowhere_a_model_or_a_reader_looks(&hosted, &lines, TYPED).await;
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declining_or_not_answering_signs_nothing_in_and_tells_the_assistant() {
    let (mut hosted, signed_in) = started().await;
    hosted.workspaces.call_private_model_addresses(true);

    // She declines on her device: nothing is kept, and her assistant hears it once.
    she_says_it_and_is_asked(&mut hosted).await;
    hosted.model.then(Reply::text("Nothing was signed in."));
    approves(&mut hosted, "decline").await;
    let told = super::resumed(&mut hosted, "declined").await;
    assert!(told.contains("declined"), "{told}");
    let kept = hosted.workspace_vault().await;
    assert!(
        kept.is_empty(),
        "the workspace's vault keeps nothing more: {kept:?}"
    );
    assert!(her_mail(&mut hosted).await.is_empty());
    assert!(signed_in.lock().unwrap().is_empty());
    hosted.page.peer.close();
}

/// The address of a person whose own mail server answers at ports its domain publishes
/// nowhere, and that server as she names it: its name, and each port after a colon.
const OWN_ADDRESS: &str = "ada@nowhere.test";
const OWN_SERVER: &str = "mail.nowhere.test";
const OWN_READS: &str = "mail.nowhere.test:1993";
const OWN_SENDS: &str = "mail.nowhere.test:1587";
/// Where a sign-in for that server belongs, as the workspace's vault names it.
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
                Reply::text("I asked you for its password. Type it where your device asks."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    assert!(told.contains(OWN_SERVER), "{told}");
    let asked = hosted.page.asks().await;
    assert_eq!(
        asked.len(),
        1,
        "a server named with its port is taken, and its person asked once: {told}"
    );
    asks_for_her_password(&asked[0], &[OWN_ADDRESS, OWN_SERVER]);

    // She types. Her server keeps it for exactly those two places, signs in and keeps the
    // account at the ports she named.
    let _ = she_types(&mut hosted, TYPED).await;
    hosted
        .model
        .then(Reply::text("Your mail account is signed in."));
    approves(&mut hosted, "approve").await;
    super::resumed(&mut hosted, "signed in").await;
    let vault = hosted.workspace_vault().await;
    assert_eq!(
        vault[0]["belongsTo"],
        serde_json::json!(OWN_PLACES),
        "{vault:?}"
    );
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
            hosted.page.asks().await.is_empty(),
            "{what}: nobody is asked for a password"
        );
    }
    assert!(her_mail(&mut hosted).await.is_empty());
    let kept = hosted.workspace_vault().await;
    assert!(
        kept.is_empty(),
        "the workspace's vault keeps nothing more: {kept:?}"
    );
    assert!(signed_in.lock().unwrap().is_empty());
    hosted.page.peer.close();
}

/// The subjects of her inbox, as her own device reads it through the host's mail action.
async fn her_inbox(hosted: &mut Hosted) -> Vec<String> {
    let fetched = hosted
        .on_device(
            (
                person_accounts::PROVIDER,
                person_accounts::RESOURCE,
                person_accounts::FETCH,
            ),
            serde_json::json!({"account": ADDRESS, "folder": "inbox"}),
        )
        .await;
    fetched["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("her inbox: {fetched}"))
        .iter()
        .map(|message| message["subject"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// With real mail servers: a stand-in with real sockets that takes [`TYPED`] alone, and holds
/// made-up mail. What she types on her own device signs her in there, her inbox is read
/// through the host's own mail action, and a message she sends herself arrives in it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_she_types_on_her_device_signs_her_in_and_her_mail_arrives() {
    use tilo_server::workspaces::hosted_use::mail_stand_in;
    let lines = logged();
    let up = mail_stand_in::started(mail_stand_in::Serving {
        names: vec!["localhost".to_owned()],
        login: ADDRESS.to_owned(),
        password: TYPED.to_owned(),
        oauth_token: None,
        imap: "127.0.0.1:0".to_owned(),
        smtp: "127.0.0.1:0".to_owned(),
        imap_wrapped: true,
        smtp_wrapped: false,
        layout: mail_stand_in::Layout::Plain,
        seeded: true,
    })
    .expect("the stand-in mail server starts");
    let (imap, smtp) = (up.imap.port(), up.smtp.port());
    let mut hosted = Hosted::start().await;
    // The stand-in answers on this machine: its operator lets this server call its own network,
    // and trusts the stand-in's certificate, which no authority signed.
    hosted.workspaces.call_private_model_addresses(true);
    hosted
        .workspaces
        .trust_mail_servers(vec![up.certificate.clone()]);
    hosted
        .workspaces
        .find_mail_servers_through(move |address, _| match address {
            ADDRESS => Ok(MailServers {
                incoming: MailServer::new("localhost", imap),
                outgoing: MailServer::new("localhost", smtp),
                found: MailFound::Published,
                username: None,
                own_sign_in: None,
            }),
            _ => Err("no mail servers are found for that address".to_owned()),
        });

    hosted
        .turn(
            SENTENCE,
            vec![
                tells("call-1"),
                Reply::text("I asked you for its password. Type it where your device asks."),
            ],
        )
        .await;
    let asked = hosted.page.asks().await;
    asks_for_her_password(&asked[0], &[ADDRESS, "localhost", "Scout"]);
    assert!(
        her_mail(&mut hosted).await.is_empty(),
        "nothing before she typed"
    );

    let _ = she_types(&mut hosted, TYPED).await;
    hosted
        .model
        .then(Reply::text("Your mail account is signed in."));
    approves(&mut hosted, "approve").await;
    let told = super::resumed(&mut hosted, "signed in").await;
    assert!(told.contains("signed in"), "{told}");
    let accounts = her_mail(&mut hosted).await;
    assert_eq!(accounts.len(), 1, "{accounts:?}");
    assert_eq!(accounts[0]["hasSecret"], true, "{}", accounts[0]);

    let inbox = her_inbox(&mut hosted).await;
    assert!(
        inbox.iter().any(|subject| subject == "Welcome to the lab"),
        "her server's mail is read with what she typed: {inbox:?}"
    );

    let begun = hosted
        .on_device(
            (
                person_accounts::PROVIDER,
                person_accounts::RESOURCE,
                person_accounts::BEGIN_SEND,
            ),
            serde_json::json!({"message": {
                "account": ADDRESS,
                "fromAddress": ADDRESS,
                "to": ADDRESS,
                "subject": "A note to myself",
                "body": "Sent through the account my assistant signed in.",
            }}),
        )
        .await;
    let send = begun["send"].as_str().unwrap_or_else(|| panic!("{begun}"));
    let sent = hosted
        .on_device(
            (
                person_accounts::PROVIDER,
                person_accounts::RESOURCE,
                person_accounts::SEND,
            ),
            serde_json::json!({"send": send}),
        )
        .await;
    assert!(sent.get("error").is_none(), "it leaves: {sent}");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let inbox = her_inbox(&mut hosted).await;
        if inbox.iter().any(|subject| subject == "A note to myself") {
            break;
        }
        assert!(Instant::now() < deadline, "her message arrived: {inbox:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    it_is_nowhere_a_model_or_a_reader_looks(&hosted, &lines, TYPED).await;
    hosted.page.peer.close();
}
