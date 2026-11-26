//! The stand-in mail server answers the sign-in this crate proves, over real sockets: it takes
//! its one login and refuses another, on ports that upgrade and on ports that are TLS from the
//! first byte. Every login here is made up for this file.

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Arc;

use base64::Engine as _;

use crate::stand_in::{Serving, Started, pem, started};

const LOGIN: &str = "ada@example.test";
/// The one password the stand-in takes here, and one it does not.
const TAKEN: &str = "made-up-for-the-mail-stand-in";
const OTHER: &str = "not-what-the-mail-stand-in-takes";
const STEP: std::time::Duration = std::time::Duration::from_secs(8);

fn stand_in(wrapped: bool) -> Started {
    started(Serving {
        names: vec!["localhost".to_owned()],
        login: LOGIN.to_owned(),
        password: TAKEN.to_owned(),
        oauth_token: None,
        imap: "127.0.0.1:0".to_owned(),
        smtp: "127.0.0.1:0".to_owned(),
        imap_wrapped: wrapped,
        smtp_wrapped: wrapped,
        layout: crate::stand_in::Layout::Plain,
        seeded: true,
    })
    .expect("the stand-in starts")
}

/// The roots that trust exactly the stand-in's certificate.
fn roots_of(up: &Started) -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(up.certificate.clone())
        .expect("its certificate is a root");
    roots
}

/// Send `command`, when there is one, and read the reply's last line.
fn replied<S: Read + Write>(server: &mut BufReader<S>, command: &str) -> String {
    if !command.is_empty() {
        let to = server.get_mut();
        write!(to, "{command}\r\n").expect("the command is sent");
        to.flush().expect("the command is sent");
    }
    loop {
        let mut line = String::new();
        assert!(
            server.read_line(&mut line).expect("a reply") > 0,
            "the stand-in left"
        );
        if line.as_bytes().get(3) != Some(&b'-') {
            return line;
        }
    }
}

#[test]
fn the_sign_in_proof_is_taken_for_the_one_login_and_refused_for_another() {
    let up = stand_in(false);
    // The account the crate's own tests sign in with, on ports that start in the clear.
    let mut account = super::email_account(up.imap.port(), up.smtp.port());
    account.config["imap_tls"] = serde_json::json!(false);
    assert_eq!(
        crate::LiveAccountTester::email_with_roots(
            &account,
            &accounts::Secret::password(TAKEN),
            roots_of(&up),
        ),
        Ok("IMAP and SMTP sign-in succeeded".into())
    );
    assert_eq!(
        crate::LiveAccountTester::email_with_roots(
            &account,
            &accounts::Secret::password(OTHER),
            roots_of(&up),
        ),
        Err("IMAP server rejected the saved credential".into())
    );
    // A program that trusts nothing of the stand-in's does not sign in to it.
    assert_eq!(
        crate::LiveAccountTester::email_with_roots(
            &account,
            &accounts::Secret::password(TAKEN),
            rustls::RootCertStore::empty(),
        ),
        Err(
            "could not establish the secure IMAP connection: the server's certificate is not trusted"
                .into()
        )
    );
    assert!(pem(&up.certificate).starts_with("-----BEGIN CERTIFICATE-----\n"));
}

#[test]
fn a_stack_signs_in_with_imap_on_tls_and_smtp_by_upgrade_at_once() {
    // What the local stack starts: the IMAP port is TLS from the first byte and
    // the SMTP port upgrades, and the settings Mail keeps for a port that is not
    // 143 say exactly that, so a made-up account signs in on both.
    let up = started(Serving {
        names: vec!["localhost".to_owned()],
        login: LOGIN.to_owned(),
        password: TAKEN.to_owned(),
        oauth_token: None,
        imap: "127.0.0.1:0".to_owned(),
        smtp: "127.0.0.1:0".to_owned(),
        imap_wrapped: true,
        smtp_wrapped: false,
        layout: crate::stand_in::Layout::Plain,
        seeded: true,
    })
    .expect("the stand-in starts");
    let account = super::email_account(up.imap.port(), up.smtp.port());
    assert_eq!(
        crate::LiveAccountTester::email_with_roots(
            &account,
            &accounts::Secret::password(TAKEN),
            roots_of(&up),
        ),
        Ok("IMAP and SMTP sign-in succeeded".into())
    );
}

#[test]
fn on_ports_that_are_tls_from_the_first_byte_both_servers_take_the_one_login() {
    let up = stand_in(true);
    let tls = Arc::new(
        tilo_net::client_config_with_roots(roots_of(&up)).expect("a client configuration"),
    );
    // IMAP, by the connection the proof itself opens.
    let asking = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("sockets and a clock");
    asking.block_on(async {
        for (password, taken) in [(OTHER, false), (TAKEN, true)] {
            let client = crate::imap::open(
                &crate::imap::Reach::any(),
                "localhost",
                up.imap.port(),
                accounts::email::Security::Tls,
                Arc::clone(&tls),
                STEP,
            )
            .await
            .expect("it greets over TLS");
            assert_eq!(client.login(LOGIN, password).await.is_ok(), taken);
        }
    });
    // SMTP, by hand: the proof takes TLS from the first byte on port 465 alone.
    let name = rustls::pki_types::ServerName::try_from("localhost").expect("a name");
    let connection = rustls::ClientConnection::new(tls, name).expect("a client");
    let tcp = std::net::TcpStream::connect(("127.0.0.1", up.smtp.port())).expect("it answers");
    let mut server = BufReader::new(rustls::StreamOwned::new(connection, tcp));
    let plain = |password: &str| {
        let token =
            base64::engine::general_purpose::STANDARD.encode(format!("\0{LOGIN}\0{password}"));
        format!("AUTH PLAIN {token}")
    };
    assert!(replied(&mut server, "").starts_with("220 "));
    assert!(replied(&mut server, "EHLO [127.0.0.1]").starts_with("250 AUTH PLAIN LOGIN"));
    assert!(replied(&mut server, &plain(OTHER)).starts_with("535 "));
    assert!(replied(&mut server, &plain(TAKEN)).starts_with("235 "));
    // It takes the mail it is handed, and keeps it.
    assert!(replied(&mut server, &plain(TAKEN)).starts_with("235 "));
    assert!(replied(&mut server, "MAIL FROM:<ada@example.test>").starts_with("250 "));
    assert!(replied(&mut server, "RCPT TO:<grace@example.test>").starts_with("250 "));
    assert!(replied(&mut server, "DATA").starts_with("354 "));
    assert!(replied(&mut server, "Subject: A note\r\n\r\nIts words\r\n.").starts_with("250 "));
    assert!(replied(&mut server, "QUIT").starts_with("221 "));
}

/// An account a provider signed in on its own page is proved with the
/// bearer token it carries: XOAUTH2 to both servers, and any other token is
/// refused.
#[test]
fn the_sign_in_proof_takes_a_bearer_token_by_xoauth2() {
    let up = started(Serving {
        names: vec!["localhost".to_owned()],
        login: LOGIN.to_owned(),
        password: TAKEN.to_owned(),
        oauth_token: Some("access-token-1".to_owned()),
        imap: "127.0.0.1:0".to_owned(),
        smtp: "127.0.0.1:0".to_owned(),
        imap_wrapped: true,
        smtp_wrapped: false,
        layout: crate::stand_in::Layout::Plain,
        seeded: true,
    })
    .expect("the stand-in starts");
    let mut account = accounts::Account::new(LOGIN, accounts::email::TYPE_ID);
    account.config = serde_json::json!({
        "email": LOGIN,
        "username": LOGIN,
        "imap_host": "localhost",
        "imap_port": up.imap.port(),
        "imap_tls": true,
        "smtp_host": "localhost",
        "smtp_port": up.smtp.port(),
        "auth": "oauth",
    });
    assert_eq!(
        crate::LiveAccountTester::email_with_roots(
            &account,
            &accounts::Secret::password("access-token-1"),
            roots_of(&up),
        )
        .unwrap(),
        "IMAP and SMTP sign-in succeeded"
    );
    assert!(
        crate::LiveAccountTester::email_with_roots(
            &account,
            &accounts::Secret::password("another-token"),
            roots_of(&up),
        )
        .is_err()
    );
}

/// A client that trusts the stand-in's certificate and nothing else.
fn trusting(up: &Started) -> Arc<rustls::ClientConfig> {
    Arc::new(tilo_net::client_config_with_roots(roots_of(up)).expect("a client configuration"))
}

#[test]
fn the_inbox_holds_made_up_mail_a_person_reads_and_searches() {
    let up = stand_in(true);
    let tls = trusting(&up);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("sockets and a clock");
    runtime.block_on(async {
        use futures_util::TryStreamExt as _;
        let client = crate::imap::open(
            &crate::imap::Reach::any(),
            "localhost",
            up.imap.port(),
            accounts::email::Security::Tls,
            tls,
            STEP,
        )
        .await
        .expect("it greets over TLS");
        let mut session = client.login(LOGIN, TAKEN).await.expect("signed in");
        let folders: Vec<String> = session
            .list(Some(""), Some("*"))
            .await
            .expect("it lists")
            .try_collect::<Vec<_>>()
            .await
            .expect("it lists")
            .iter()
            .map(|folder| folder.name().to_owned())
            .collect();
        assert_eq!(folders, vec!["INBOX".to_owned()]);
        let selected = session.select("INBOX").await.expect("it selects");
        assert_eq!(selected.exists, 3);
        let fetched: Vec<async_imap::types::Fetch> = session
            .fetch("1:3", "(UID FLAGS INTERNALDATE BODY.PEEK[])")
            .await
            .expect("it fetches")
            .try_collect()
            .await
            .expect("it fetches");
        assert_eq!(fetched.len(), 3);
        let newest = fetched
            .iter()
            .find(|fetch| fetch.uid == Some(3))
            .expect("the newest message");
        let body = String::from_utf8_lossy(newest.body().expect("its words"));
        assert!(body.contains("Welcome to the lab"), "{body}");
        assert!(
            newest.internal_date().is_some(),
            "a message says when it came"
        );
        assert!(
            !fetched[0]
                .flags()
                .any(|flag| matches!(flag, async_imap::types::Flag::Seen)),
            "nothing is read before the person reads it"
        );
        // A person searches for an older one by its words.
        let found: Vec<u32> = session
            .uid_search("SUBJECT \"Quarterly\"")
            .await
            .expect("it searches")
            .into_iter()
            .collect();
        assert_eq!(found, vec![1]);
        // Reading marks it read, as a mail program does.
        session
            .uid_store("1", "+FLAGS.SILENT (\\Seen)")
            .await
            .expect("it stores")
            .try_collect::<Vec<_>>()
            .await
            .expect("it stores");
        let flags: Vec<async_imap::types::Fetch> = session
            .uid_fetch("1", "(UID FLAGS)")
            .await
            .expect("it fetches")
            .try_collect()
            .await
            .expect("it fetches");
        assert!(
            flags[0]
                .flags()
                .any(|flag| matches!(flag, async_imap::types::Flag::Seen)),
            "the message the person read is read"
        );
    });
}

#[test]
fn a_message_handed_to_smtp_for_its_login_arrives_in_the_inbox() {
    let up = stand_in(true);
    let tls = trusting(&up);
    let name = rustls::pki_types::ServerName::try_from("localhost").expect("a name");
    let connection = rustls::ClientConnection::new(tls.clone(), name).expect("a client");
    let tcp = std::net::TcpStream::connect(("127.0.0.1", up.smtp.port())).expect("it answers");
    let mut server = BufReader::new(rustls::StreamOwned::new(connection, tcp));
    let token = base64::engine::general_purpose::STANDARD.encode(format!("\0{LOGIN}\0{TAKEN}"));
    assert!(replied(&mut server, "").starts_with("220 "));
    assert!(replied(&mut server, "EHLO [127.0.0.1]").starts_with("250 "));
    assert!(replied(&mut server, &format!("AUTH PLAIN {token}")).starts_with("235 "));
    assert!(replied(&mut server, "MAIL FROM:<grace@example.test>").starts_with("250 "));
    // To the stand-in's own login, whose inbox it holds.
    assert!(replied(&mut server, "RCPT TO:<ada@example.test>").starts_with("250 "));
    assert!(replied(&mut server, "DATA").starts_with("354 "));
    assert!(
        replied(
            &mut server,
            "From: Grace <grace@example.test>\r\nSubject: Re: Welcome to the lab\r\n\r\nThank you.\r\n."
        )
        .starts_with("250 ")
    );
    assert!(replied(&mut server, "QUIT").starts_with("221 "));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("sockets and a clock");
    runtime.block_on(async {
        use futures_util::TryStreamExt as _;
        let client = crate::imap::open(
            &crate::imap::Reach::any(),
            "localhost",
            up.imap.port(),
            accounts::email::Security::Tls,
            tls,
            STEP,
        )
        .await
        .expect("it greets over TLS");
        let mut session = client.login(LOGIN, TAKEN).await.expect("signed in");
        let selected = session.select("INBOX").await.expect("it selects");
        assert_eq!(
            selected.exists, 4,
            "the message it was handed is in the inbox"
        );
        let fetched: Vec<async_imap::types::Fetch> = session
            .uid_fetch("4", "(UID BODY.PEEK[])")
            .await
            .expect("it fetches")
            .try_collect()
            .await
            .expect("it fetches");
        let body = String::from_utf8_lossy(fetched[0].body().expect("its words"));
        assert!(body.contains("Subject: Re: Welcome to the lab"), "{body}");
    });
}
