//! End-to-end proof of the account → Mail → engine loop, on the default
//! (network-free) build.
//!
//! An email account of the workspace's record, its password in the device's
//! vault, is read back by Mail's own reader and then driven through the real
//! account engine: the same `EngineRequest`/`EngineEvent` channel the live UI
//! posts to. The offline (stub) backend answers Connect/Fetch and builds a
//! genuine MIME on Send, so this exercises the whole request/response plumbing
//! plus the real parse (incoming) and build (outgoing) paths. The live
//! IMAP/SMTP socket loop is proven separately by the `--features live` engine
//! tests.

use mail_engine::account::accounts_of;
use mail_engine::engine::{EngineEvent, EngineRequest};
use mail_engine::model::{AccountConfig, Addr, Draft, Folder};
use mail_engine::transport::{build_mime, parse_eml, parse_fetched};

fn recv(
    rx: &std::sync::mpsc::Receiver<EngineEvent>,
) -> Result<EngineEvent, std::sync::mpsc::RecvTimeoutError> {
    rx.recv_timeout(tilo_test_env::hang_bound())
}

fn assert_send_builds_real_mime(loaded: &AccountConfig, draft: &Draft) {
    let from = Addr::new(loaded.display_name.clone(), loaded.email.clone());
    let mime = build_mime(&tilo_content::ContentStore::memory(), &from, draft).expect("build MIME");
    let text = String::from_utf8_lossy(&mime);
    assert!(
        text.contains("Subject: Loop check"),
        "subject header present: {text}"
    );
    assert!(
        text.contains("ada@analytical.example"),
        "From address present in the payload"
    );
    assert!(
        text.contains("you@example.test"),
        "To recipient present in the payload"
    );
    assert!(
        text.contains("cc@example.test"),
        "Cc recipient present in the payload"
    );
    assert!(
        text.contains("This travels through the engine."),
        "body present in the payload"
    );
    let round = parse_eml(&mime, 0, Folder::Sent).expect("parse built MIME");
    assert_eq!(round.subject, "Loop check");
    assert_eq!(round.from.email, "ada@analytical.example");
    assert!(
        round.to.iter().any(|a| a.email == "you@example.test"),
        "To recipient survives"
    );
    assert!(
        round.cc.iter().any(|a| a.email == "cc@example.test"),
        "Cc recipient survives"
    );
}

#[test]
fn account_from_the_shared_store_drives_the_engine_end_to_end() {
    // 1) An account is configured and kept in the workspace's accounts the way Mail writes it:
    let mut cfg = AccountConfig {
        display_name: "Ada Lovelace".into(),
        email: "ada@analytical.example".into(),
        username: "ada@analytical.example".into(),
        imap_host: "imap.analytical.example".into(),
        imap_port: 993,
        smtp_host: "smtp.analytical.example".into(),
        smtp_port: 465,
        ..Default::default()
    };
    cfg.password = "app-password".into();
    let account = mail_engine::account::Account::new(cfg);
    let mut replica = tilo_resource::Replica::<accounts::person::AccountList>::new()
        .in_app(accounts::person::ACCOUNTS_OWNER, "Settings");
    replica
        .apply(accounts::person::AccountList::saving(&account.to_stored()))
        .expect("the record keeps the account");

    // 2) Mail reads that exact account back from the record, without its
    //    password: the session releases that to whoever signs in.
    let person = replica.state().unwrap();
    let loaded = accounts_of(&person);
    assert_eq!(
        loaded.len(),
        1,
        "Mail reads back the one configured account"
    );
    let mut loaded = loaded[0].config.clone();
    assert_eq!(loaded.email, "ada@analytical.example");
    assert_eq!(loaded.imap_host, "imap.analytical.example");
    assert!(
        loaded.password.is_empty(),
        "Mail reads no password from the device"
    );
    loaded.password = "app-password".into();

    // 3) Drive the account engine with that loaded account over its real
    //    channel — exactly the requests the app posts. The offline stub backend
    //    is used explicitly so the loop is deterministic and network-free
    //    regardless of which backend the default `spawn()` would select.
    let (tx, rx) = mail_engine::engine::spawn_with(Box::new(mail_engine::engine::StubEngine::new(
        tilo_content::ContentStore::memory(),
    )));

    let connect = EngineRequest::connect(loaded.clone());
    let connect_id = connect.request_id();
    tx.send(connect).unwrap();
    assert!(matches!(
        recv(&rx).expect("engine answered within the timeout"),
        EngineEvent::Error { request_id, kind: mail_engine::engine::EngineRequestKind::Connect, context, .. }
            if request_id == connect_id && context == "offline"
    ));
    assert!(matches!(
        recv(&rx).expect("engine answered within the timeout"),
        EngineEvent::Completed { request_id, kind: mail_engine::engine::EngineRequestKind::Connect }
            if request_id == connect_id
    ));

    let fetch = EngineRequest::fetch(loaded.clone(), Folder::Inbox);
    let fetch_id = fetch.request_id();
    tx.send(fetch).unwrap();
    assert!(matches!(
        recv(&rx).expect("engine answered within the timeout"),
        EngineEvent::Error { request_id, kind: mail_engine::engine::EngineRequestKind::Fetch, context, .. }
            if request_id == fetch_id && context == "offline"
    ));
    assert!(matches!(
        recv(&rx).expect("engine answered within the timeout"),
        EngineEvent::Completed { request_id, kind: mail_engine::engine::EngineRequestKind::Fetch }
            if request_id == fetch_id
    ));

    // 4a) A compose produces a well-formed SMTP payload: the Send request builds
    //     real RFC 5322 MIME and reports its wire size.
    let draft = Draft {
        to: "you@example.test".into(),
        cc: "cc@example.test".into(),
        subject: "Loop check".into(),
        body: "This travels through the engine.".into(),
        ..Default::default()
    };
    let send = EngineRequest::send(loaded.clone(), Box::new(draft.clone()), Vec::new());
    let send_id = send.request_id();
    tx.send(send).unwrap();
    assert!(matches!(
        recv(&rx).expect("engine answered within the timeout"),
        EngineEvent::Error { request_id, kind: mail_engine::engine::EngineRequestKind::Send, context, message }
            if request_id == send_id && context == "send" && message.contains("validated") && message.contains("did not send")
    ));
    assert!(matches!(
        recv(&rx).expect("engine answered within the timeout"),
        EngineEvent::Completed { request_id, kind: mail_engine::engine::EngineRequestKind::Send }
            if request_id == send_id
    ));
    assert_send_builds_real_mime(&loaded, &draft);

    // 4b) A fetch parses a message: the exact decode a live UID FETCH runs, with
    //     the server's \Seen flag applied.
    let raw = b"From: Charles Babbage <babbage@engine.example>\r\n\
To: ada@analytical.example\r\n\
Subject: Difference Engine notes\r\n\
Date: Fri, 01 Jan 2021 09:30:00 +0000\r\n\
\r\n\
The mill and the store are ready.\r\n";
    let msg =
        parse_fetched(raw, Folder::Inbox, true, false, None, 0).expect("parse a fetched message");
    assert_eq!(msg.subject, "Difference Engine notes");
    assert_eq!(msg.from.email, "babbage@engine.example");
    assert!(msg.body.contains("mill and the store"));
    assert!(msg.read, "the server's \\Seen flag maps to read");
}
