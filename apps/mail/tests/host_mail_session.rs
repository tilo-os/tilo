//! The session the host carries for an account it keeps, against the real
//! IMAP and SMTP servers this app's own tests use: a folder is read whole, a
//! message is sent, and a server is told what changed about one.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_actions::remote::person_accounts::{FetchMail, MarkMail, SendMail};
use tilo_mail_session::{Attachment, LiveMailSession, MailSession};

use mail::fake_imap::{FakeImap, Layout};
use mail::fake_servers::{FakeSmtp, server_identity};

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
    let (imap, smtp) = runtime
        .block_on(async {
            (
                FakeImap::start(Layout::Gmail, tls.clone()).await,
                FakeSmtp::start(tls).await,
            )
        })
        .expect("the test servers start");
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
            &[],
            None,
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

/// A message with the id its mail program gave it.
fn filed_once() -> SendMail {
    SendMail {
        account: "ada@example.test".into(),
        from_name: Some("Ada".into()),
        from_address: "ada@example.test".into(),
        to: "Bob <bob@example.test>".into(),
        subject: "Filed once".into(),
        body: "Hello Bob.".into(),
        message_id: Some("filed-once@example.test".into()),
        ..SendMail::default()
    }
}

/// A sent message is filed in the Sent folder of a server that files none:
/// in a folder the host makes on a plain server that holds none yet, and in
/// the folder a server keeps sent mail in under its own name. Sent again
/// under the same id it is not filed twice, and a server that files what it
/// sends is left to it.
#[test]
fn a_sent_message_is_filed_once_where_the_server_files_none() {
    let runtime = ui::rt::app();
    for (layout, sent_folder, copies) in [
        (Layout::Plain, "Sent", 1),
        (Layout::Outlook, "Sent Items", 1),
        (Layout::Gmail, "[Gmail]/Sent Mail", 0),
    ] {
        let (tls, certificate) = server_identity();
        let (imap, smtp) = runtime
            .block_on(async move {
                (
                    FakeImap::start(layout, tls.clone()).await,
                    FakeSmtp::start(tls).await,
                )
            })
            .expect("the test servers start");
        let account = account(imap.port, smtp.port);
        let secret = accounts::Secret::password("fixture-password");
        let session = LiveMailSession::also_trusting(vec![certificate]);
        for round in 1..=2 {
            let sent = session
                .send(&account, &secret, &filed_once(), &[], None)
                .expect("the message is sent");
            assert_eq!(sent.unfiled, None, "{layout:?}");
            assert_eq!(smtp.sent().len(), round, "{layout:?}: it left each time");
            let filed = imap.messages(sent_folder);
            assert_eq!(filed.len(), copies, "{layout:?}: round {round}");
            for copy in &filed {
                assert!(copy.flags.iter().any(|flag| flag == "\\Seen"));
                assert!(String::from_utf8_lossy(&copy.raw).contains("Subject: Filed once"));
            }
        }
        // Only a server that held no Sent folder was given one.
        assert_eq!(
            imap.folders().iter().any(|name| name == "Sent"),
            matches!(layout, Layout::Plain),
            "{layout:?}: {:?}",
            imap.folders()
        );
    }
}

/// A message leaves even when its copy cannot be filed, and the answer says
/// why the Sent folder holds none.
#[test]
fn a_message_leaves_though_its_copy_cannot_be_filed() {
    let runtime = ui::rt::app();
    let (tls, certificate) = server_identity();
    let (imap, smtp) = runtime
        .block_on(async move {
            (
                FakeImap::start(Layout::Plain, tls.clone()).await,
                FakeSmtp::start(tls).await,
            )
        })
        .expect("the test servers start");
    imap.refuse_appends();
    let account = account(imap.port, smtp.port);
    let secret = accounts::Secret::password("fixture-password");
    let session = LiveMailSession::also_trusting(vec![certificate]);

    let sent = session
        .send(&account, &secret, &filed_once(), &[], None)
        .expect("the message is sent");
    assert_eq!(smtp.sent().len(), 1, "the message left");
    assert!(imap.messages("Sent").is_empty());
    let reason = sent.unfiled.expect("the answer says no copy was filed");
    assert!(reason.contains("Sent would not take the copy"), "{reason}");
}

/// A plain server on a port that starts in the clear, as Dovecot serves 143:
/// the host upgrades the connection before it signs in, and reads the inbox.
#[test]
fn a_host_signs_in_to_a_plain_server_only_once_the_connection_is_secure() {
    let runtime = ui::rt::app();
    let (tls, certificate) = server_identity();
    let imap = runtime
        .block_on(FakeImap::start_starttls(Layout::Plain, tls))
        .expect("the test server starts");
    imap.append(
        "INBOX",
        "From: Ana <ana@veeza.example>\r\nTo: Ada <ada@example.test>\r\n\
Subject: Over STARTTLS\r\nMessage-ID: <upgraded@example.test>\r\n\
Content-Type: text/plain\r\n\r\nHello Ada.\r\n",
        &[],
    );
    let mut account = account(imap.port, 465);
    account.config["imap_tls"] = serde_json::json!(false);
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
    assert_eq!(fetched.messages.len(), 1);
    assert_eq!(fetched.messages[0].subject, "Over STARTTLS");
    assert_eq!(imap.logins(), 1);
    assert_eq!(
        imap.cleartext_logins(),
        0,
        "nothing signs in before the connection is secure"
    );
}

/// A message sent with a file and an invitation reads back from the server
/// with both intact: the host wrote the MIME from the bytes it was handed,
/// as a page that builds none hands them.
#[test]
fn a_message_with_a_file_reads_back_with_the_file_intact() {
    let runtime = ui::rt::app();
    let (tls, certificate) = server_identity();
    let (imap, smtp) = runtime
        .block_on(async {
            (
                FakeImap::start(Layout::Gmail, tls.clone()).await,
                FakeSmtp::start(tls).await,
            )
        })
        .expect("the test servers start");
    let account = account(imap.port, smtp.port);
    let secret = accounts::Secret::password("fixture-password");
    let session = LiveMailSession::also_trusting(vec![certificate]);
    let pdf = b"%PDF-1.4\nfixture attachment\n%%EOF\n".to_vec();
    let invitation = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n";

    session
        .send(
            &account,
            &secret,
            &SendMail {
                account: "ada@example.test".into(),
                from_name: Some("Ada".into()),
                from_address: "ada@example.test".into(),
                to: "Bob <bob@example.test>".into(),
                subject: "Contract".into(),
                body: "Attached.".into(),
                ..SendMail::default()
            },
            &[Attachment {
                filename: "contract.pdf".into(),
                mime: "application/pdf".into(),
                bytes: pdf.clone(),
            }],
            Some(invitation),
        )
        .expect("the message is sent");

    let sent = smtp.sent();
    assert_eq!(sent.len(), 1);
    let raw = String::from_utf8_lossy(&sent[0]).to_string();
    let lowered = raw.to_ascii_lowercase();
    assert!(lowered.contains("filename=\"contract.pdf\""), "{lowered}");
    assert!(lowered.contains("application/pdf"), "{lowered}");
    assert!(lowered.contains("invitation.ics"), "{lowered}");
    assert!(lowered.contains("text/calendar"), "{lowered}");

    // What the server took is what its folder holds: read it back as mail.
    imap.append("INBOX", &raw, &[]);
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
    let message = fetched
        .messages
        .iter()
        .find(|message| message.subject == "Contract")
        .expect("the sent message is in the inbox");
    let file = message
        .attachments
        .iter()
        .find(|attachment| attachment.filename == "contract.pdf")
        .expect("the file came with the message");
    assert_eq!(file.mime, "application/pdf");
    assert_eq!(file.size, pdf.len() as u64, "the file's bytes are intact");
}
