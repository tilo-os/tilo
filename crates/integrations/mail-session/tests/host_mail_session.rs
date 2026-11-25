//! The session the host carries for an account it keeps, against the stand-in mail
//! server: a folder is read whole, a message is sent, and a server is told what
//! changed about one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use tilo_mail_session::{Attachment, Leaving, LiveMailSession, MailSession};
use tilo_mail_session::{FetchMail, MarkMail, ReadMail, SearchMail, SendMail};

use tilo_account_probe::stand_in::{Layout, Serving, Started, started};

use std::sync::{Arc, Mutex, PoisonError};
use tilo_mail_session::FetchedMessage;
use tilo_mail_session::{InboxHeard, InboxSeen, WatchPace};

/// What a watch told, as a test reads it.
#[derive(Default)]
struct Told {
    looks: Mutex<Vec<(Vec<String>, InboxSeen)>>,
    lost: Mutex<Vec<String>>,
}

impl InboxHeard for Told {
    fn arrived(&self, messages: Vec<FetchedMessage>, seen: InboxSeen) {
        let subjects = messages
            .into_iter()
            .map(|message| message.subject)
            .collect();
        self.looks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((subjects, seen));
    }

    fn lost(&self, reason: &str) {
        self.lost
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(reason.to_owned());
    }
}

impl Told {
    /// The subjects of everything told so far, in the order it was told.
    fn subjects(&self) -> Vec<String> {
        self.looks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .flat_map(|(subjects, _)| subjects.clone())
            .collect()
    }

    /// Where the watch has read up to, once it looked.
    fn seen(&self) -> Option<InboxSeen> {
        self.looks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .map(|(_, seen)| *seen)
    }

    fn losses(&self) -> usize {
        self.lost
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// A pace a test does not wait long for.
fn quick() -> WatchPace {
    let short = std::time::Duration::from_millis(50);
    WatchPace {
        renew: std::time::Duration::from_secs(60),
        poll: short,
        retry: short,
        retry_most: std::time::Duration::from_millis(200),
    }
}

/// A host watching an inbox with no window open tells each message that
/// arrives once and nothing that was there before, stops when the watch is
/// let go, and a watch started where it stopped tells what arrived in
/// between.
#[test]
fn a_watched_inbox_tells_each_arrival_once_and_nothing_that_was_there() {
    let imap = served(Layout::Plain, true);
    let certificate = imap.certificate.clone();
    let arrives = |subject: &str| {
        imap.append(
            "INBOX",
            &note("Ana <ana@veeza.example>", subject, subject, "Hello."),
            &[],
        );
    };
    arrives("Was there");
    let account = account(imap.imap.port(), 465);
    let secret = accounts::Secret::password("fixture-password");
    let session = LiveMailSession::also_trusting(vec![certificate]).pacing(quick());
    let told = Arc::new(Told::default());

    let watching = session
        .watch(&account, &secret, None, told.clone())
        .expect("the watch starts");
    tilo_test_env::wait_until("the inbox to be watched", || imap.idling() > 0);
    assert!(
        told.subjects().is_empty(),
        "what was there before is no arrival"
    );
    assert_eq!(told.seen().map(|seen| seen.uid), Some(1));

    arrives("First");
    tilo_test_env::wait_until("the arrival to be told", || told.subjects().len() == 1);
    arrives("Second");
    arrives("Third");
    tilo_test_env::wait_until("the next two to be told", || told.subjects().len() == 3);
    assert_eq!(told.subjects(), ["First", "Second", "Third"]);
    let seen = told.seen().expect("the watch looked");
    assert_eq!(seen.uid, 4);
    assert_eq!(imap.logins(), 1, "one connection watches");

    drop(watching);
    tilo_test_env::wait_until("the watch to stop", || imap.idling() == 0);
    arrives("While nobody watched");

    // A watch started where the last one stopped tells what arrived since.
    let again = Arc::new(Told::default());
    let _watching = session
        .watch(&account, &secret, Some(seen), again.clone())
        .expect("the watch starts");
    tilo_test_env::wait_until("what arrived in between to be told", || {
        again.subjects() == ["While nobody watched"]
    });

    // A folder its server numbered anew starts over: nothing in it is an
    // arrival.
    let renumbered = Arc::new(Told::default());
    let _other = session
        .watch(
            &account,
            &secret,
            Some(InboxSeen {
                uid_validity: seen.uid_validity.saturating_add(1),
                uid: 1,
            }),
            renumbered.clone(),
        )
        .expect("the watch starts");
    tilo_test_env::wait_until("the renumbered folder to be looked at", || {
        renumbered.seen().is_some()
    });
    assert!(renumbered.subjects().is_empty());
    assert_eq!(renumbered.seen().map(|seen| seen.uid), Some(5));
}

/// A watch whose server drops it comes back by itself, tells what arrived
/// while it was away once, and keeps watching.
#[test]
fn a_watch_comes_back_after_its_server_drops_it_and_misses_nothing() {
    let imap = served(Layout::Plain, true);
    let certificate = imap.certificate.clone();
    let arrives = |subject: &str| {
        imap.append(
            "INBOX",
            &note("Ana <ana@veeza.example>", subject, subject, "Hello."),
            &[],
        );
    };
    let account = account(imap.imap.port(), 465);
    let secret = accounts::Secret::password("fixture-password");
    let session = LiveMailSession::also_trusting(vec![certificate]).pacing(quick());
    let told = Arc::new(Told::default());
    let _watching = session
        .watch(&account, &secret, None, told.clone())
        .expect("the watch starts");
    tilo_test_env::wait_until("the inbox to be watched", || imap.idling() > 0);

    imap.drop_connections();
    arrives("While away");
    tilo_test_env::wait_until("the watch to come back and tell it", || {
        told.subjects() == ["While away"]
    });
    assert!(told.losses() >= 1, "the loss was told");
    assert!(imap.logins() >= 2, "the watch signed in again");

    tilo_test_env::wait_until("the inbox to be watched again", || imap.idling() > 0);
    arrives("After");
    tilo_test_env::wait_until("the watch to tell what arrives next", || {
        told.subjects() == ["While away", "After"]
    });
}

/// A server that cannot tell of a change is asked again and again instead.
#[test]
fn a_server_that_cannot_tell_of_a_change_is_asked_again_and_again() {
    let imap = served(Layout::Plain, true);
    let certificate = imap.certificate.clone();
    imap.without_idle();
    let account = account(imap.imap.port(), 465);
    let secret = accounts::Secret::password("fixture-password");
    let session = LiveMailSession::also_trusting(vec![certificate]).pacing(quick());
    let told = Arc::new(Told::default());
    let _watching = session
        .watch(&account, &secret, None, told.clone())
        .expect("the watch starts");
    tilo_test_env::wait_until("the inbox to be looked at", || told.seen().is_some());

    imap.append(
        "INBOX",
        &note("Ana <ana@veeza.example>", "Asked for", "asked", "Hello."),
        &[],
    );
    tilo_test_env::wait_until("the arrival to be found by asking", || {
        told.subjects() == ["Asked for"]
    });
    assert_eq!(
        imap.idling(),
        0,
        "nothing idles on a server that cannot tell"
    );
    assert_eq!(imap.logins(), 1, "the one connection is asked again");
}

/// A stand-in mail server laid out as `layout` that takes Ada's sign-in: its IMAP port is TLS
/// from the first byte when `wrapped`, and upgrades otherwise.
fn served(layout: Layout, wrapped: bool) -> Started {
    started(Serving {
        layout,
        imap_wrapped: wrapped,
        ..Serving::local("ada@example.test", "fixture-password")
    })
    .expect("the test servers start")
}

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
    let imap = served(Layout::Gmail, true);
    let smtp = imap.clone();
    let certificate = imap.certificate.clone();
    imap.append(
        "INBOX",
        "From: Ana <ana@veeza.example>\r\nTo: Ada <ada@example.test>\r\n\
Subject: Contract for review\r\nMessage-ID: <one@example.test>\r\n\
Content-Type: text/plain\r\n\r\nHello Ada,\r\n\r\nThe draft is ready.\r\n",
        &[],
    );
    let account = account(imap.imap.port(), smtp.smtp.port());
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
            &Leaving::read(SendMail {
                account: "ada@example.test".into(),
                from_name: Some("Ada".into()),
                from_address: "ada@example.test".into(),
                to: "Bob <bob@example.test>".into(),
                subject: "Re: Contract for review".into(),
                body: "Thanks, reviewing today.".into(),
                ..SendMail::default()
            })
            .expect("the message reads"),
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

/// One message as a mail program sends it.
fn note(from: &str, subject: &str, id: &str, body: &str) -> String {
    format!(
        "From: {from}\nTo: Ada <ada@example.test>\nSubject: {subject}\n\
Message-ID: <{id}@example.test>\nContent-Type: text/plain\n\n{body}\n"
    )
}

/// A host reads a folder a page at a time, reads one message, asks the
/// server what it holds and keeps a draft there, all over the one session
/// its first request signed in with.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one session's life with its server: every request is served over the connection the first one opened"
)]
fn a_host_pages_reads_searches_and_drafts_over_one_session() {
    let imap = served(Layout::Plain, true);
    let certificate = imap.certificate.clone();
    for n in 1..=60 {
        imap.append(
            "INBOX",
            &note(
                "Ana <ana@veeza.example>",
                &format!("Note {n}"),
                &format!("note-{n}"),
                "Nothing new.",
            ),
            &[],
        );
    }
    let asked = imap
        .append(
            "INBOX",
            &note(
                "Jonas <jonas@northzone.example>",
                "Cap table",
                "cap-table",
                "Could you send the cap table?",
            ),
            &[],
        )
        .expect("the inbox holds it");
    let account = account(imap.imap.port(), 465);
    let secret = accounts::Secret::password("fixture-password");
    let session = LiveMailSession::also_trusting(vec![certificate]);
    let inbox = |asking: FetchMail| {
        session
            .fetch(&account, &secret, &asking)
            .expect("the inbox is read")
    };
    let of_inbox = || FetchMail {
        account: "ada@example.test".into(),
        folder: "inbox".into(),
        ..FetchMail::default()
    };

    // The newest page, then the page before its oldest message.
    let newest = inbox(of_inbox());
    assert!(newest.reset && newest.has_older);
    assert_eq!(newest.messages.len(), 50);
    let oldest = newest.messages.iter().map(|m| m.uid).min().unwrap();
    assert_eq!(oldest, 12);
    let older = inbox(FetchMail {
        known_uid_validity: Some(newest.uid_validity),
        before_uid: Some(oldest),
        ..of_inbox()
    });
    assert!(!older.reset, "an older page replaces nothing");
    assert!(!older.has_older, "the folder begins there");
    let mut uids: Vec<u32> = older.messages.iter().map(|m| m.uid).collect();
    uids.sort_unstable();
    assert_eq!(uids, (1..=11).collect::<Vec<_>>());

    // One message, by where its server holds it.
    let reading = |uid: u32, uid_validity: u32| ReadMail {
        account: "ada@example.test".into(),
        folder: "inbox".into(),
        uid,
        uid_validity,
    };
    let read = session
        .read(&account, &secret, &reading(asked, newest.uid_validity))
        .expect("the message is read");
    assert_eq!(read.subject, "Cap table");
    assert!(read.body.contains("send the cap table"));
    assert!(!read.partial);
    // One that is gone, and a folder numbered anew since, are said.
    let gone = session
        .read(&account, &secret, &reading(999, newest.uid_validity))
        .unwrap_err();
    assert!(gone.contains("no longer in its folder"), "{gone}");
    let renumbered = session
        .read(&account, &secret, &reading(asked, newest.uid_validity + 1))
        .unwrap_err();
    assert!(renumbered.contains("folder changed"), "{renumbered}");

    // The server is asked what it holds, and every part of a search has to
    // match. A plain server keeps no archive, so only its inbox answers.
    let searching = |search: SearchMail| {
        session
            .search(
                &account,
                &secret,
                &SearchMail {
                    account: "ada@example.test".into(),
                    ..search
                },
            )
            .expect("the server is searched")
    };
    let found = searching(SearchMail {
        from: Some("northzone".into()),
        text: Some("cap table".into()),
        ..SearchMail::default()
    });
    assert_eq!(found.folders.len(), 1);
    assert_eq!(found.folders[0].folder, "inbox");
    let uids = |folder: usize, found: &tilo_mail_session::FoundMail| {
        found.folders[folder]
            .messages
            .iter()
            .map(|m| m.uid)
            .collect::<Vec<_>>()
    };
    assert_eq!(uids(0, &found), [asked]);
    let nothing = searching(SearchMail {
        from: Some("northzone".into()),
        subject: Some("Note 3".into()),
        ..SearchMail::default()
    });
    assert!(nothing.folders[0].messages.is_empty());
    let newest_five = searching(SearchMail {
        subject: Some("Note".into()),
        limit: Some(5),
        ..SearchMail::default()
    });
    assert_eq!(uids(0, &newest_five), [60, 59, 58, 57, 56]);
    let nameless = session.search(
        &account,
        &secret,
        &SearchMail {
            account: "ada@example.test".into(),
            ..SearchMail::default()
        },
    );
    assert!(nameless.is_err(), "a search names what it looks for");

    // A draft is kept on the server, in a folder made for it, read and
    // marked as a draft, and nothing is sent.
    session
        .save_draft(
            &account,
            &secret,
            &SendMail {
                account: "ada@example.test".into(),
                from_address: "ada@example.test".into(),
                to: "bob@example.test".into(),
                subject: "Not yet".into(),
                body: "Still thinking.".into(),
                message_id: Some("not-yet@example.test".into()),
                ..SendMail::default()
            },
            &[],
            None,
        )
        .expect("the draft is kept");
    let drafts = imap.messages("Drafts");
    assert_eq!(drafts.len(), 1);
    assert!(drafts[0].flags.iter().any(|flag| flag == "\\Draft"));
    assert!(String::from_utf8_lossy(&drafts[0].raw).contains("Subject: Not yet"));

    // A copy written over is deleted by its `Message-ID`, the only thing a
    // window knows of a draft's earlier copy, and deleting it again is no
    // failure: it is gone.
    let retiring = MarkMail {
        account: "ada@example.test".into(),
        folder: "drafts".into(),
        message_id: Some("<not-yet@example.test>".into()),
        delete: true,
        ..MarkMail::default()
    };
    session
        .mark(&account, &secret, &retiring)
        .expect("the earlier copy is deleted");
    assert!(imap.messages("Drafts").is_empty());
    session
        .mark(&account, &secret, &retiring)
        .expect("a copy gone already is deleted");

    assert_eq!(
        imap.logins(),
        1,
        "every request was served over the session the first one opened"
    );
}

/// What a folder's messages say is answered as far as one answer holds, so
/// it always fits the frame it travels in: the newest whole, the rest by
/// their start, and one of those read on its own is whole again.
#[test]
fn a_folder_too_long_for_one_answer_is_answered_by_its_newest_and_the_start_of_the_rest() {
    let imap = served(Layout::Plain, true);
    let certificate = imap.certificate.clone();
    let long = "All work and no play makes a long message.\n".repeat(9000);
    assert!(long.len() > 300 * 1024);
    for n in 1..=3 {
        imap.append(
            "INBOX",
            &note(
                "Ana <ana@veeza.example>",
                &format!("Long {n}"),
                &format!("long-{n}"),
                &long,
            ),
            &[],
        );
    }
    let account = account(imap.imap.port(), 465);
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
    assert_eq!(fetched.messages.len(), 3);
    let by_uid = |uid: u32| {
        fetched
            .messages
            .iter()
            .find(|m| m.uid == uid)
            .expect("each message is answered")
    };
    assert!(!by_uid(3).partial, "the newest is answered whole");
    assert!(by_uid(3).body.len() > 300 * 1024);
    for uid in [1, 2] {
        assert!(
            by_uid(uid).partial,
            "message {uid} is answered by its start"
        );
        assert!(by_uid(uid).body.len() <= 2 * 1024);
        assert!(by_uid(uid).body.starts_with("All work and no play"));
        assert_eq!(by_uid(uid).subject, format!("Long {uid}"));
    }
    assert!(
        serde_json::to_vec(&fetched).unwrap().len() < 1024 * 1024,
        "the answer fits the frame an action's answer travels in"
    );

    let whole = session
        .read(
            &account,
            &secret,
            &ReadMail {
                account: "ada@example.test".into(),
                folder: "inbox".into(),
                uid: 1,
                uid_validity: fetched.uid_validity,
            },
        )
        .expect("the message is read");
    assert!(!whole.partial);
    assert!(whole.body.len() > 300 * 1024);
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
    for (layout, sent_folder, copies) in [
        (Layout::Plain, "Sent", 1),
        (Layout::Outlook, "Sent Items", 1),
        (Layout::Gmail, "[Gmail]/Sent Mail", 0),
    ] {
        let imap = served(layout, true);
        let smtp = imap.clone();
        let certificate = imap.certificate.clone();
        let account = account(imap.imap.port(), smtp.smtp.port());
        let secret = accounts::Secret::password("fixture-password");
        let session = LiveMailSession::also_trusting(vec![certificate]);
        for round in 1..=2 {
            let sent = session
                .send(
                    &account,
                    &secret,
                    &Leaving::read(filed_once()).expect("the message reads"),
                    &[],
                    None,
                )
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
    let imap = served(Layout::Plain, true);
    let smtp = imap.clone();
    let certificate = imap.certificate.clone();
    imap.refuse_appends();
    let account = account(imap.imap.port(), smtp.smtp.port());
    let secret = accounts::Secret::password("fixture-password");
    let session = LiveMailSession::also_trusting(vec![certificate]);

    let sent = session
        .send(
            &account,
            &secret,
            &Leaving::read(filed_once()).expect("the message reads"),
            &[],
            None,
        )
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
    let imap = served(Layout::Plain, false);
    let certificate = imap.certificate.clone();
    imap.append(
        "INBOX",
        "From: Ana <ana@veeza.example>\r\nTo: Ada <ada@example.test>\r\n\
Subject: Over STARTTLS\r\nMessage-ID: <upgraded@example.test>\r\n\
Content-Type: text/plain\r\n\r\nHello Ada.\r\n",
        &[],
    );
    let mut account = account(imap.imap.port(), 465);
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
    let imap = served(Layout::Gmail, true);
    let smtp = imap.clone();
    let certificate = imap.certificate.clone();
    let account = account(imap.imap.port(), smtp.smtp.port());
    let secret = accounts::Secret::password("fixture-password");
    let session = LiveMailSession::also_trusting(vec![certificate]);
    let pdf = b"%PDF-1.4\nfixture attachment\n%%EOF\n".to_vec();
    let invitation = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n";

    session
        .send(
            &account,
            &secret,
            &Leaving::read(SendMail {
                account: "ada@example.test".into(),
                from_name: Some("Ada".into()),
                from_address: "ada@example.test".into(),
                to: "Bob <bob@example.test>".into(),
                subject: "Contract".into(),
                body: "Attached.".into(),
                ..SendMail::default()
            })
            .expect("the message reads"),
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
