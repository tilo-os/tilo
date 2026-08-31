//! End-to-end proof of the account → Mail → engine loop, on the default
//! (network-free) build.
//!
//! An email account persisted into the shared account store + secret vault is
//! read back by Mail's own loader, then driven through the real account engine:
//! the same `EngineRequest`/`EngineEvent` channel the live UI posts to. The
//! offline (stub) backend answers Connect/Fetch and builds a genuine MIME on
//! Send, so this exercises the whole request/response plumbing plus the real
//! parse (incoming) and build (outgoing) paths. The live IMAP/SMTP socket loop
//! is proven separately by the `--features live` engine tests.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use mail::account::AccountStore;
use mail::engine::{EngineEvent, EngineRequest};
use mail::model::{AccountConfig, Addr, Draft, Folder};
use mail::transport::{build_mime, parse_eml, parse_fetched};

/// A process-unique temp config root so the test never collides with a real
/// `~/.config/qos` or a parallel run.
fn unique_root() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("qos-mail-engine-loop-{}-{}", std::process::id(), n))
}

fn recv(
    rx: &std::sync::mpsc::Receiver<EngineEvent>,
) -> Result<EngineEvent, std::sync::mpsc::RecvTimeoutError> {
    rx.recv_timeout(Duration::from_secs(5))
}

/// Step 4a of the end-to-end loop test: a compose produces a well-formed SMTP
/// payload. Build the same draft's MIME (the exact function the engine's Send
/// uses) and check it directly, then parse it back. Note: two independent
/// builds are NOT byte-identical — RFC 5322 requires a unique Message-ID and
/// a current Date, which mail-builder auto-generates per call, so the payload
/// is verified by structure and content, not by comparing the length of a
/// second build to the engine's. The exact bytes the engine puts on the wire
/// are asserted by the `--features live` SMTP socket test, which captures the
/// server-side DATA.
fn assert_send_builds_real_mime(loaded: &AccountConfig, draft: &Draft) {
    let from = Addr::new(loaded.display_name.clone(), loaded.email.clone());
    let mime = build_mime(&from, draft).expect("build MIME");
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
        text.contains("you@qos.example"),
        "To recipient present in the payload"
    );
    assert!(
        text.contains("cc@qos.example"),
        "Cc recipient present in the payload"
    );
    assert!(
        text.contains("This travels through the engine."),
        "body present in the payload"
    );
    // And it parses back cleanly as a real RFC 5322 message.
    let round = parse_eml(&mime, 0, Folder::Sent).expect("parse built MIME");
    assert_eq!(round.subject, "Loop check");
    assert_eq!(round.from.email, "ada@analytical.example");
    assert!(
        round.to.iter().any(|a| a.email == "you@qos.example"),
        "To recipient survives"
    );
    assert!(
        round.cc.iter().any(|a| a.email == "cc@qos.example"),
        "Cc recipient survives"
    );
}

#[test]
fn account_from_the_shared_store_drives_the_engine_end_to_end() {
    let root = unique_root();
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    // 1) An account is configured and persisted the way Settings/Mail write it:
    //    into the shared store, with the password sealed in the vault.
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
    AccountStore::single(cfg)
        .persist_in(&root)
        .expect("persist account to the shared store");

    // 2) Mail loads that exact account back through its own loader, rehydrating
    //    the password from the vault (never from the plaintext store).
    let store = AccountStore::load_in(&root);
    assert_eq!(store.len(), 1, "Mail reads back the one configured account");
    let loaded = store.active_config();
    assert_eq!(loaded.email, "ada@analytical.example");
    assert_eq!(loaded.imap_host, "imap.analytical.example");
    assert_eq!(
        loaded.password, "app-password",
        "password rehydrates from the vault"
    );

    // 3) Drive the account engine with that loaded account over its real
    //    channel — exactly the requests the app posts. The offline stub backend
    //    is used explicitly so the loop is deterministic and network-free
    //    regardless of which backend the default `spawn()` would select.
    let (tx, rx) = mail::engine::spawn_with(Box::new(mail::engine::StubEngine));

    tx.send(EngineRequest::Connect {
        account: loaded.clone(),
    })
    .unwrap();
    match recv(&rx).expect("engine answered within the timeout") {
        EngineEvent::Connected { account_id } => {
            assert_eq!(account_id, "ada@analytical.example");
        }
        other => panic!("expected Connected, got {other:?}"),
    }

    tx.send(EngineRequest::Fetch {
        account: loaded.clone(),
        folder: Folder::Inbox,
    })
    .unwrap();
    match recv(&rx).expect("engine answered within the timeout") {
        EngineEvent::Fetched { folder, .. } => assert_eq!(folder, Folder::Inbox),
        other => panic!("expected Fetched, got {other:?}"),
    }

    // 4a) A compose produces a well-formed SMTP payload: the Send request builds
    //     real RFC 5322 MIME and reports its wire size.
    let draft = Draft {
        to: "you@qos.example".into(),
        cc: "cc@qos.example".into(),
        subject: "Loop check".into(),
        body: "This travels through the engine.".into(),
        ..Default::default()
    };
    tx.send(EngineRequest::Send {
        account: loaded.clone(),
        draft: Box::new(draft.clone()),
    })
    .unwrap();
    let wire = match recv(&rx).expect("engine answered within the timeout") {
        EngineEvent::Sent { wire_bytes } => wire_bytes,
        other => panic!("expected Sent, got {other:?}"),
    };
    assert!(
        wire > 0,
        "the engine built and measured a real MIME payload on Send"
    );
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

    let _ = std::fs::remove_dir_all(&root);
}
