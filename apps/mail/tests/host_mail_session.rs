//! The session the host carries for an account it keeps, against the real
//! IMAP and SMTP servers this app's own tests use: a folder is read whole, a
//! message is sent, and a server is told what changed about one.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_account_probe::{LiveMailSession, MailSession};
use tilo_actions::remote::person_accounts::{FetchMail, MarkMail, SendMail};

use crate::fake_imap::{FakeImap, Layout};
use crate::fake_servers::{FakeSmtp, server_identity};

/// The account that reaches `imap` and `smtp`.
fn account(imap_port: u16, smtp_port: u16) -> accounts::Account {
    let mut account = accounts::Account::new("ada@example.test", "email");
    account.config = serde_json::json!({
        "email": "ada@example.test",
        "username": "ada@example.test",
        "imap_host": "localhost",
        "imap_port": imap_port,
        "smtp_host": "localhost",
        "smtp_port": smtp_port,
        "auth": "password"
    });
    account
}

#[test]
fn a_host_reads_sends_and_marks_through_real_mail_servers() {
    let runtime = ui::rt::app();
    let (tls, certificate) = server_identity();
    let (imap, smtp) = runtime.block_on(async {
        (
            FakeImap::start(Layout::Gmail, tls.clone()).await,
            FakeSmtp::start(tls).await,
        )
    });
    imap.append(
        "INBOX",
        "From: Ana <ana@veeza.example>\r\nTo: Ada <ada@example.test>\r\n\
Subject: Contract for review\r\nMessage-ID: <one@example.test>\r\n\
Content-Type: text/plain\r\n\r\nHello Ada,\r\n\r\nThe draft is ready.\r\n",
        &[],
    );
    let account = account(imap.port, smtp.port);
    let secret = accounts::Secret::password("fixture-password");
    let session = LiveMailSession::also_trusting(vec![certificate]);

    let fetched = session
        .fetch(
            &account,
            &secret,
            &FetchMail {
                account: "ada@example.test".into(),
                folder: "inbox".into(),
                ..FetchMail::default()
            },
        )
        .expect("the inbox is read");
    assert!(fetched.reset);
    assert_eq!(fetched.messages.len(), 1);
    assert_eq!(fetched.messages[0].subject, "Contract for review");
    assert_eq!(fetched.messages[0].from.email, "ana@veeza.example");
    assert!(fetched.messages[0].body.contains("draft is ready"));
    let uid = fetched.messages[0].uid;

    session
        .send(
            &account,
            &secret,
            &SendMail {
                account: "ada@example.test".into(),
                from_name: Some("Ada".into()),
                from_address: "ada@example.test".into(),
                to: "Bob <bob@example.test>".into(),
                subject: "Re: Contract for review".into(),
                body: "Thanks, reviewing today.".into(),
                ..SendMail::default()
            },
        )
        .expect("the message is sent");
    let sent = smtp.sent();
    assert_eq!(sent.len(), 1);
    let mail = String::from_utf8_lossy(&sent[0]).to_ascii_lowercase();
    assert!(mail.contains("subject: re: contract for review"), "{mail}");
    assert!(mail.contains("bob@example.test"), "{mail}");

    session
        .mark(
            &account,
            &secret,
            &MarkMail {
                account: "ada@example.test".into(),
                folder: "inbox".into(),
                uid,
                uid_validity: fetched.uid_validity,
                read: Some(true),
                starred: Some(true),
                ..MarkMail::default()
            },
        )
        .expect("the server is told");
    let stored = imap.messages("INBOX");
    assert!(stored[0].flags.iter().any(|flag| flag == "\\Seen"));
    assert!(stored[0].flags.iter().any(|flag| flag == "\\Flagged"));
}
