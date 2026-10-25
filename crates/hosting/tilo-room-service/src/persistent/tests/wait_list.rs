//! The door's wait list: leaving an address, taking it off, confirming it
//! and the mail it sends.

use super::*;

/// Leaves `email` at the door from `address`, answering as the relay
/// with `answer` (given the command's request) when the door asks it.
async fn wait_once(
    persistent: &Arc<Persistent>,
    relay: &mut Relay,
    address: &str,
    email: &str,
    answer: impl FnOnce(String) -> RoomEvent,
) -> (
    std::result::Result<WaitListAnswer, WaitRefusal>,
    Option<String>,
) {
    let (answer, contact, _) = wait_digest(persistent, relay, address, email, answer).await;
    (answer, contact)
}

/// [`wait_once`], with the removal digest the relay was sent.
async fn wait_digest(
    persistent: &Arc<Persistent>,
    relay: &mut Relay,
    address: &str,
    email: &str,
    answer: impl FnOnce(String) -> RoomEvent,
) -> (
    std::result::Result<WaitListAnswer, WaitRefusal>,
    Option<String>,
    String,
) {
    let waiting = tokio::spawn({
        let persistent = Arc::clone(persistent);
        let address = ip(address);
        let email = email.to_owned();
        async move {
            persistent
                .wait(
                    address,
                    WaitListRequest {
                        email,
                        language: String::new(),
                    },
                )
                .await
        }
    });
    let OperatorCommand::WaitList {
        request,
        contact,
        removal_sha256,
        ..
    } = relay.command().await
    else {
        panic!("expected a wait-list request");
    };
    relay.report(&answer(request)).await;
    (waiting.await.unwrap(), Some(contact), removal_sha256)
}

/// Asks the door to take off the entry of `secret` from `address`,
/// answering as the relay does, whatever the secret: `Unlisted`. The
/// door's answer, and the digest the relay was sent.
async fn unlist_once(
    persistent: &Arc<Persistent>,
    relay: &mut Relay,
    address: &str,
    secret: &str,
) -> (std::result::Result<(), WaitRefusal>, String) {
    let leaving = tokio::spawn({
        let persistent = Arc::clone(persistent);
        let address = ip(address);
        let secret = secret.to_owned();
        async move { persistent.unlist(address, UnlistRequest { secret }).await }
    });
    let OperatorCommand::Unlist {
        request,
        removal_sha256,
    } = relay.command().await
    else {
        panic!("expected a wait-list removal");
    };
    relay
        .report(&RoomEvent::Unlisted {
            request,
            told: None,
        })
        .await;
    (leaving.await.unwrap(), removal_sha256)
}

#[tokio::test]
async fn whoever_left_an_address_gets_a_secret_the_room_sees_only_the_hash_of() {
    let (persistent, mut relay, _dir) = inviting().await;
    let (answer, _, digest) = wait_digest(
        &persistent,
        &mut relay,
        "203.0.113.1",
        "ada@example.org",
        |request| RoomEvent::WaitListed {
            request,
            added: true,
            mail: false,
        },
    )
    .await;
    let secret = answer.unwrap().removal;
    assert_eq!(secret.len(), 64, "256 random bits, as hex");
    let bytes: [u8; 32] = hex32(&secret).unwrap();
    assert_eq!(digest, removal_digest(&bytes));
    assert_ne!(digest, secret);

    // The same address left again gets a secret of its own.
    let (again, _, other) = wait_digest(
        &persistent,
        &mut relay,
        "203.0.113.1",
        "ADA@example.org",
        |request| RoomEvent::WaitListed {
            request,
            added: false,
            mail: false,
        },
    )
    .await;
    assert_ne!(again.unwrap().removal, secret);
    assert_ne!(other, digest);

    // The secret takes it off; a wrong one is answered the same, and
    // the room is only ever sent hashes.
    let (left, sent) = unlist_once(&persistent, &mut relay, "203.0.113.1", &secret).await;
    assert!(left.is_ok());
    assert_eq!(sent, digest);
    let wrong = "ab".repeat(32);
    let (wrong_left, sent) = unlist_once(&persistent, &mut relay, "203.0.113.1", &wrong).await;
    assert_eq!(format!("{wrong_left:?}"), format!("{left:?}"));
    assert_eq!(sent, removal_digest(&[0xab; 32]));

    // What is no secret at all is refused before the room is asked.
    for text in ["", "secret", &"AB".repeat(32), &"ab".repeat(31)] {
        assert!(matches!(
            persistent
                .unlist(
                    ip("203.0.113.1"),
                    UnlistRequest {
                        secret: text.to_owned()
                    }
                )
                .await,
            Err(WaitRefusal::NotASecret)
        ));
    }
}

#[tokio::test]
async fn taking_entries_off_is_counted_per_network_and_a_failure_does_not_count() {
    let (persistent, mut relay, _dir) = inviting().await;
    // The room failing to write: unavailable, and not counted.
    let failing = tokio::spawn({
        let persistent = Arc::clone(&persistent);
        async move {
            persistent
                .unlist(
                    ip("203.0.113.9"),
                    UnlistRequest {
                        secret: "cd".repeat(32),
                    },
                )
                .await
        }
    });
    let OperatorCommand::Unlist { request, .. } = relay.command().await else {
        panic!("expected a wait-list removal");
    };
    relay
        .report(&RoomEvent::WaitListFailed {
            request,
            reason: "disk full".into(),
        })
        .await;
    assert!(matches!(
        failing.await.unwrap(),
        Err(WaitRefusal::Unavailable)
    ));
    for n in 0..JoinRates::WAIT_LIST_REMOVALS.per_ten_minutes {
        let secret = hex::encode([u8::try_from(n).unwrap(); 32]);
        let (left, _) = unlist_once(&persistent, &mut relay, "203.0.113.9", &secret).await;
        assert!(left.is_ok());
    }
    assert!(matches!(
        persistent
            .unlist(
                ip("203.0.113.9"),
                UnlistRequest {
                    secret: "ef".repeat(32)
                }
            )
            .await,
        Err(WaitRefusal::TooMany { .. })
    ));
    // Another network is not held back.
    let (left, _) = unlist_once(&persistent, &mut relay, "198.51.100.4", &"ef".repeat(32)).await;
    assert!(left.is_ok());
}

#[tokio::test]
async fn an_address_left_at_the_door_goes_to_the_room_as_typed() {
    let (persistent, mut relay, _dir) = inviting().await;
    assert_eq!(
        persistent.wait_list(),
        Some(WaitListDoor {
            contact: Some("hello@example.org"),
            mail: false,
        })
    );
    let (answer, contact) = wait_once(
        &persistent,
        &mut relay,
        "203.0.113.1",
        " Ada@Example.ORG ",
        |request| RoomEvent::WaitListed {
            request,
            added: true,
            mail: false,
        },
    )
    .await;
    assert_eq!(contact.as_deref(), Some("Ada@Example.ORG"));
    assert_eq!(answer.unwrap().email, "Ada@Example.ORG");
    // The same address again reads as kept: nobody learns whose it is.
    let (again, _) = wait_once(
        &persistent,
        &mut relay,
        "203.0.113.1",
        "ada@example.org",
        |request| RoomEvent::WaitListed {
            request,
            added: false,
            mail: false,
        },
    )
    .await;
    assert_eq!(again.unwrap().email, "ada@example.org");
    let (full, _) = wait_once(
        &persistent,
        &mut relay,
        "203.0.113.2",
        "bo@example.org",
        |request| RoomEvent::WaitListRefused {
            request,
            reason: "the wait list is full".into(),
            full: true,
        },
    )
    .await;
    assert!(matches!(full, Err(WaitRefusal::Full)));
}

#[tokio::test]
async fn a_bad_address_or_one_network_too_often_is_refused_before_the_room_is_asked() {
    let (persistent, mut relay, _dir) = inviting().await;
    assert!(matches!(
        persistent
            .wait(
                ip("203.0.113.1"),
                WaitListRequest {
                    email: "ada".into(),
                    language: String::new(),
                }
            )
            .await,
        Err(WaitRefusal::Invalid(_))
    ));
    for n in 0..JoinRates::WAIT_LIST.per_ten_minutes {
        let (answer, _) = wait_once(
            &persistent,
            &mut relay,
            "203.0.113.1",
            &format!("p{n}@example.org"),
            |request| RoomEvent::WaitListed {
                request,
                added: true,
                mail: false,
            },
        )
        .await;
        answer.unwrap();
    }
    assert!(matches!(
        persistent
            .wait(
                ip("203.0.113.1"),
                WaitListRequest {
                    email: "more@example.org".into(),
                    language: String::new(),
                }
            )
            .await,
        Err(WaitRefusal::TooMany { .. })
    ));
}

#[tokio::test]
async fn an_open_room_keeps_no_wait_list() {
    let (persistent, _relay, _dir) = persistent(200, 1000).await;
    assert_eq!(persistent.wait_list(), None);
    assert!(matches!(
        persistent
            .wait(
                ip("203.0.113.1"),
                WaitListRequest {
                    email: "ada@example.org".into(),
                    language: String::new(),
                }
            )
            .await,
        Err(WaitRefusal::NoWaitList)
    ));
}

/// [`inviting`], where the door mails through `mailer`.
async fn mailing(
    mailer: Arc<crate::mail::FakeMailer>,
) -> (Arc<Persistent>, Relay, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut shared = Shared::with_key(
        dir.path().to_path_buf(),
        [7; 32],
        200,
        1000,
        (JoinRates::GUESTS, JoinRates::SPECTATORS),
    )
    .unwrap();
    shared.invite_only = true;
    shared.wait_list = true;
    shared.mail = Some(crate::mail::DoorMail::new(
        mailer,
        "https://tilo.example/",
        "Lounge",
    ));
    let (stdin, relay_stdin) = tokio::io::duplex(64 * 1024);
    let (mut relay_stdout, stdout) = tokio::io::duplex(64 * 1024);
    let started = RoomEvent::Started {
        version: room_operator::VERSION,
        origin: "https://rooms.example".into(),
        plain_origin: None,
        user_public_key: "ee".repeat(32),
        certificate_sha256: None,
        allowed_origins: Vec::new(),
        room: Some("room-uuid".into()),
    };
    relay_stdout
        .write_all(format!("{}\n", started.to_line()).as_bytes())
        .await
        .unwrap();
    let persistent = Persistent::attach_for_test(Arc::new(shared), Box::new(stdin), stdout).await;
    let relay = Relay {
        commands: BufReader::new(relay_stdin).lines(),
        events: relay_stdout,
    };
    (persistent, relay, dir)
}

/// The mails `mailer` was asked to send by now.
async fn mailed(mailer: &crate::mail::FakeMailer, count: usize) -> Vec<crate::mail::Mail> {
    taken_in("the mail", || mailer.sent.lock().unwrap().len() >= count).await;
    mailer.sent.lock().unwrap().clone()
}

/// The secret (hex) a mailed link carries in `parameter`.
fn carried(text: &str, parameter: &str) -> String {
    text.split(&format!("?{parameter}="))
        .nth(1)
        .unwrap()
        .chars()
        .take(64)
        .collect()
}

#[tokio::test]
async fn a_mailing_door_mails_a_confirmation_link_the_room_holds_only_hashes_of() {
    let mailer = Arc::new(crate::mail::FakeMailer::default());
    let (persistent, mut relay, _dir) = mailing(Arc::clone(&mailer)).await;
    assert_eq!(persistent.wait_list().map(|door| door.mail), Some(true));
    let waiting = tokio::spawn({
        let persistent = Arc::clone(&persistent);
        async move {
            persistent
                .wait(
                    ip("203.0.113.1"),
                    WaitListRequest {
                        email: "Ada@Example.org".into(),
                        language: "de-DE".into(),
                    },
                )
                .await
        }
    });
    let OperatorCommand::WaitList {
        request,
        removal_sha256,
        confirm_sha256: Some(confirm_sha256),
        language,
        ..
    } = relay.command().await
    else {
        panic!("expected a wait-list request to confirm");
    };
    assert_eq!(language, "de");
    relay
        .report(&RoomEvent::WaitListed {
            request,
            added: true,
            mail: true,
        })
        .await;
    let answer = waiting.await.unwrap().unwrap();
    assert!(answer.mail);
    let [mail] = mailed(&mailer, 1).await.try_into().unwrap();
    assert_eq!(mail.to, "Ada@Example.org");
    assert!(mail.subject.contains("Warteliste"));
    let token = carried(&mail.text, "wait-list-confirm");
    assert_eq!(carried(&mail.text, "wait-list-remove"), answer.removal);
    assert_eq!(removal_digest(&hex32(&token).unwrap()), confirm_sha256);
    assert_eq!(
        removal_digest(&hex32(&answer.removal).unwrap()),
        removal_sha256
    );

    // The link confirms it: the room is sent the hash of its secret.
    let confirming = tokio::spawn({
        let persistent = Arc::clone(&persistent);
        let token = token.clone();
        async move {
            persistent
                .confirm(ip("203.0.113.7"), ConfirmRequest { token })
                .await
        }
    });
    let OperatorCommand::Confirm {
        request,
        confirm_sha256: sent,
    } = relay.command().await
    else {
        panic!("expected a confirmation");
    };
    assert_eq!(sent, confirm_sha256);
    relay
        .report(&RoomEvent::Confirmed {
            request,
            confirmed: true,
        })
        .await;
    assert!(confirming.await.unwrap().unwrap());

    // The door asks where it stands.
    let asking = tokio::spawn({
        let persistent = Arc::clone(&persistent);
        let secret = answer.removal.clone();
        async move {
            persistent
                .standing(ip("203.0.113.1"), UnlistRequest { secret })
                .await
        }
    });
    let OperatorCommand::Standing { request, .. } = relay.command().await else {
        panic!("expected a standing lookup");
    };
    relay
        .report(&RoomEvent::Standing {
            request,
            standing: WaitListStanding::Listed,
        })
        .await;
    assert_eq!(asking.await.unwrap().unwrap(), WaitListStanding::Listed);
}

#[tokio::test]
async fn an_address_left_again_within_the_hour_reads_the_same_and_is_not_mailed_again() {
    let mailer = Arc::new(crate::mail::FakeMailer::default());
    let (persistent, mut relay, _dir) = mailing(Arc::clone(&mailer)).await;
    let again = tokio::spawn({
        let persistent = Arc::clone(&persistent);
        async move {
            persistent
                .wait(
                    ip("203.0.113.1"),
                    WaitListRequest {
                        email: "ada@example.org".into(),
                        language: String::new(),
                    },
                )
                .await
        }
    });
    let OperatorCommand::WaitList { request, .. } = relay.command().await else {
        panic!("expected a wait-list request");
    };
    relay
        .report(&RoomEvent::WaitListed {
            request,
            added: false,
            mail: false,
        })
        .await;
    assert!(again.await.unwrap().unwrap().mail);
    tokio::task::yield_now().await;
    assert!(
        mailer.sent.lock().unwrap().is_empty(),
        "the room said not to"
    );
}

#[tokio::test]
async fn a_member_s_invitation_and_word_of_a_removal_are_mailed_from_the_door() {
    let mailer = Arc::new(crate::mail::FakeMailer::default());
    let (persistent, mut relay, _dir) = mailing(Arc::clone(&mailer)).await;
    let to = WaitListMailbox {
        contact: "bo@example.org".into(),
        language: "en".into(),
    };
    // A link elsewhere is not mailed; one on the door is.
    relay
        .report(&RoomEvent::InviteMail {
            to: to.clone(),
            link: "https://evil.example/?invite=AAEC".into(),
            removal: "cd".repeat(32),
        })
        .await;
    relay
        .report(&RoomEvent::InviteMail {
            to: to.clone(),
            link: "https://tilo.example/#/desktop/room/x?invite=AAEC".into(),
            removal: "cd".repeat(32),
        })
        .await;
    let [invite] = mailed(&mailer, 1).await.try_into().unwrap();
    assert_eq!(invite.to, "bo@example.org");
    assert!(
        invite
            .text
            .contains("https://tilo.example/#/desktop/room/x?invite=AAEC")
    );
    assert_eq!(carried(&invite.text, "wait-list-remove"), "cd".repeat(32));

    // Its removal link takes the address off, and the room says where
    // to tell it so.
    let leaving = tokio::spawn({
        let persistent = Arc::clone(&persistent);
        async move {
            persistent
                .unlist(
                    ip("203.0.113.1"),
                    UnlistRequest {
                        secret: "cd".repeat(32),
                    },
                )
                .await
        }
    });
    let OperatorCommand::Unlist { request, .. } = relay.command().await else {
        panic!("expected a wait-list removal");
    };
    relay
        .report(&RoomEvent::Unlisted {
            request,
            told: Some(to),
        })
        .await;
    assert!(leaving.await.unwrap().is_ok());
    let mails = mailed(&mailer, 2).await;
    assert_eq!(mails[1].to, "bo@example.org");
    assert!(mails[1].subject.contains("off the"));
}
