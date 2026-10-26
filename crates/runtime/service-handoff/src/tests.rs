use super::*;

const APP: &str = "so.tilo.notes";
const ROOM: &str = "room-7";

fn app_json() -> String {
    format!(r#"{{"protocol":1,"app_id":"{APP}"}}"#)
}

fn room_json() -> String {
    format!(r#"{{"protocol":1,"app_id":"{APP}","workspace_id":"{ROOM}"}}"#)
}

fn blob_json() -> String {
    format!(r#"{{"protocol":1,"principal":"app","room":"{ROOM}","app_id":"{APP}"}}"#)
}

/// The bytes a contract must put on the wire for `json`: its magic, the
/// frame carrying exactly that JSON text, and nothing else. The text is
/// spelled out by each test, so a changed key, order, prefix or magic fails
/// here rather than in the service that reads it.
fn pinned(contract: &Contract, json: &str) -> Vec<u8> {
    let mut out = contract.magic.to_vec();
    out.extend_from_slice(&contract.framing.encode(json.as_bytes()).unwrap());
    out
}

fn raw(contract: &Contract, json: &str) -> Vec<u8> {
    pinned(contract, json)
}

#[test]
fn every_contract_reads_back_exactly_the_bytes_it_writes() {
    let cases: [(&Contract, Handoff, String); 6] = [
        (&COLLAB, Handoff::room(APP, ROOM), room_json()),
        (&COMPAT, Handoff::room(APP, ROOM), room_json()),
        (
            &BLOBS,
            Handoff::Blob(BlobScope::Session),
            r#"{"protocol":1,"principal":"session"}"#.into(),
        ),
        (&APP_HOST, Handoff::app(APP), app_json()),
        (&USH, Handoff::app(APP), app_json()),
        (&MODEL_GATEWAY, Handoff::app(APP), app_json()),
    ];
    for (contract, handoff, json) in &cases {
        let bytes = contract.encode(handoff).expect("encode");
        assert_eq!(bytes, pinned(contract, json), "{}", contract.service);
        assert_eq!(
            contract.read(&mut bytes.as_slice()).expect("read"),
            *handoff,
            "{}",
            contract.service
        );
    }
    assert_eq!(
        CONTRACTS.len(),
        cases.len(),
        "a contract has no case above, or a case names no contract"
    );
}

/// The raw prefixes are read by services that do not share this code path
/// (a raw Cap'n Proto diagnostic client, a socket-activation test), so they
/// are pinned as bytes: magic, then the length in this service's byte order.
#[test]
fn the_magic_and_length_prefix_bytes_are_pinned() {
    let ush = USH.encode(&Handoff::app(APP)).unwrap();
    assert_eq!(ush[..8], *b"QUS1\x27\0\0\0");
    assert_eq!(&ush[8..], app_json().as_bytes());

    let app_host = APP_HOST.encode(&Handoff::app(APP)).unwrap();
    assert_eq!(app_host[..8], *b"QAH1\0\0\0\x27");
    assert_eq!(&app_host[8..], app_json().as_bytes());

    let collab = COLLAB.encode(&Handoff::room(APP, ROOM)).unwrap();
    assert_eq!(collab[..8], *b"QCL1\x3f\0\0\0");
    assert_eq!(&collab[8..], room_json().as_bytes());

    let compat = COMPAT.encode(&Handoff::room(APP, ROOM)).unwrap();
    assert_eq!(compat[..4], [0x3f, 0, 0, 0]);
    assert_eq!(&compat[4..], room_json().as_bytes());

    let gateway = MODEL_GATEWAY.encode(&Handoff::app(APP)).unwrap();
    assert_eq!(gateway[..4], [0x27, 0, 0, 0]);
    assert_eq!(&gateway[4..], app_json().as_bytes());

    let blob = BLOBS
        .encode(&Handoff::Blob(BlobScope::App {
            room: ROOM.into(),
            app_id: APP.into(),
        }))
        .unwrap();
    assert_eq!(blob[..4], *b"QBL1");
    assert_eq!(&blob[8..], blob_json().as_bytes());
}

/// A handoff that arrives one byte per read is still one handoff: the
/// launcher's write and the service's read may be split by the scheduler.
#[test]
fn a_fragmented_handoff_reads_whole() {
    struct Trickle<'a> {
        bytes: &'a [u8],
    }
    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some((&byte, rest)) = self.bytes.split_first() else {
                return Ok(0);
            };
            if buf.is_empty() {
                return Ok(0);
            }
            self.bytes = rest;
            buf[0] = byte;
            Ok(1)
        }
    }
    let bytes = USH.encode(&Handoff::app(APP)).unwrap();
    let mut reader = Trickle { bytes: &bytes };
    assert_eq!(USH.read(&mut reader).unwrap(), Handoff::app(APP));
}

#[test]
fn a_length_beyond_the_bound_is_refused_before_it_is_read() {
    for contract in [&USH, &APP_HOST, &COLLAB, &COMPAT, &MODEL_GATEWAY, &BLOBS] {
        let mut bytes = contract.magic.to_vec();
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        let error = contract.read(&mut bytes.as_slice()).unwrap_err();
        assert!(
            matches!(error, HandoffError::Frame(FrameError::TooLarge(_))),
            "{}: {error:?}",
            contract.service
        );
    }
}

#[test]
fn an_oversized_envelope_is_refused_by_the_writer() {
    let long = "a".repeat(APP_HOST_HANDOFF_FRAMING.max_len());
    let error = APP_HOST
        .encode(&Handoff::app(long))
        .expect_err("a handoff larger than its service's bound must not be written");
    assert!(
        matches!(error, HandoffError::Frame(FrameError::PayloadTooLarge(_))),
        "{error:?}"
    );
}

#[test]
fn another_protocol_number_is_refused() {
    let bytes = raw(&USH, r#"{"protocol":2,"app_id":"so.tilo.notes"}"#);
    let error = USH.read(&mut bytes.as_slice()).unwrap_err();
    assert!(
        matches!(
            error,
            HandoffError::Protocol {
                found: 2,
                expected: 1
            }
        ),
        "{error:?}"
    );
}

#[test]
fn a_connection_that_starts_with_something_else_is_not_a_handoff() {
    let bytes = USH.encode(&Handoff::app(APP)).unwrap();
    let error = BLOBS.read(&mut bytes.as_slice()).unwrap_err();
    assert!(
        matches!(error, HandoffError::Marker),
        "a marker that shares its first byte but not the rest is corrupt, not another protocol: {error:?}"
    );

    let first_message = br#"{"op":"open-session"}"#.to_vec();
    let error = USH.read(&mut first_message.as_slice()).unwrap_err();
    match error {
        HandoffError::NotHandoff { carried } => assert_eq!(carried, b"{"),
        other => panic!("expected the first message to be carried, got {other:?}"),
    }

    let error = USH.read(&mut std::io::empty()).unwrap_err();
    assert!(matches!(error, HandoffError::Missing), "{error:?}");

    let error = USH.read(&mut b"QU".as_slice()).unwrap_err();
    assert!(matches!(error, HandoffError::Marker), "{error:?}");
}

#[test]
fn an_envelope_of_another_shape_is_refused() {
    for (contract, handoff) in [
        (&USH, Handoff::room(APP, ROOM)),
        (&APP_HOST, Handoff::Blob(BlobScope::Session)),
        (&BLOBS, Handoff::app(APP)),
        (&COLLAB, Handoff::app(APP)),
    ] {
        assert!(
            matches!(
                contract.encode(&handoff).unwrap_err(),
                HandoffError::Envelope
            ),
            "{} accepted another service's envelope",
            contract.service
        );
    }
}

#[test]
fn an_envelope_the_contract_does_not_define_is_refused() {
    for json in [
        r#"{"protocol":1,"app_id":"so.tilo.notes","extra":true}"#,
        r#"{"protocol":1}"#,
        r#"{"protocol":1,"app_id":""}"#,
        r#"{"protocol":1,"app_id":7}"#,
        "not json",
    ] {
        let bytes = raw(&USH, json);
        assert!(
            matches!(
                USH.read(&mut bytes.as_slice()),
                Err(HandoffError::Malformed)
            ),
            "{json} must be refused"
        );
    }
    for json in [
        r#"{"protocol":1,"app_id":"so.tilo.notes"}"#,
        r#"{"protocol":1,"app_id":"so.tilo.notes","workspace_id":""}"#,
    ] {
        let bytes = raw(&COLLAB, json);
        assert!(
            matches!(
                COLLAB.read(&mut bytes.as_slice()),
                Err(HandoffError::Malformed)
            ),
            "{json} must be refused"
        );
    }
}

#[test]
fn an_empty_identifier_is_refused_by_the_writer() {
    for handoff in [
        Handoff::app(""),
        Handoff::room("", ROOM),
        Handoff::room(APP, ""),
    ] {
        let contract = if matches!(handoff, Handoff::Room { .. }) {
            &COLLAB
        } else {
            &USH
        };
        assert!(
            matches!(contract.encode(&handoff), Err(HandoffError::Malformed)),
            "{handoff:?} must not be written"
        );
    }
}

#[test]
fn a_blob_handoff_fixes_exactly_one_valid_principal() {
    let app = BlobScope::App {
        room: ROOM.into(),
        app_id: APP.into(),
    };
    let handoff = Handoff::Blob(app);
    let bytes = BLOBS.encode(&handoff).unwrap();
    assert_eq!(BLOBS.read(&mut bytes.as_slice()).unwrap(), handoff);

    for bad in [
        r#"{"protocol":2,"principal":"session"}"#,
        r#"{"protocol":1,"principal":"session","room":"r"}"#,
        r#"{"protocol":1,"principal":"app","room":"..","app_id":"a"}"#,
        r#"{"protocol":1,"principal":"app","room":"a/b","app_id":"a"}"#,
        r#"{"protocol":1,"principal":"app","app_id":"a"}"#,
        r#"{"protocol":1,"principal":"root"}"#,
        r#"{"protocol":1,"principal":"session","extra":true}"#,
    ] {
        let bytes = raw(&BLOBS, bad);
        assert!(
            BLOBS.read(&mut bytes.as_slice()).is_err(),
            "{bad} must be refused"
        );
    }

    assert!(
        matches!(
            BLOBS.encode(&Handoff::Blob(BlobScope::App {
                room: "a/b".into(),
                app_id: APP.into(),
            })),
            Err(HandoffError::Malformed)
        ),
        "a scope component that aliases a path must not be written"
    );
    assert!(valid_scope_component("so.tilo.notes"));
    assert!(valid_scope_component("room-7"));
    assert!(!valid_scope_component(""));
    assert!(!valid_scope_component(".."));
    assert!(!valid_scope_component(&"a".repeat(MAX_SCOPE_COMPONENT + 1)));
}

/// Executing-task custody is one field on the room envelope, and its absence
/// keeps the legacy bytes.
#[test]
fn a_room_handoff_asks_for_executing_custody_only_when_named() {
    let plain = COLLAB.encode(&Handoff::room(APP, ROOM)).unwrap();
    assert_eq!(&plain[8..], room_json().as_bytes());

    let custody = Handoff::room(APP, ROOM).with_executing_peer();
    let bytes = COLLAB.encode(&custody).unwrap();
    assert_eq!(
        &bytes[8..],
        format!(
            r#"{{"protocol":1,"app_id":"{APP}","workspace_id":"{ROOM}","executing_peer":true}}"#
        )
        .as_bytes()
    );
    assert_eq!(COLLAB.read(&mut bytes.as_slice()).unwrap(), custody);
}

#[test]
fn the_table_holds_every_service_once_under_its_socket_name() {
    for (index, entry) in CONTRACTS.iter().enumerate() {
        assert!(
            CONTRACTS
                .iter()
                .skip(index + 1)
                .all(|other| other.service != entry.service),
            "{} appears twice",
            entry.service
        );
        let found = contract(entry.service).expect("every contract is found by name");
        assert_eq!(found.service, entry.service);
        assert_eq!(found.magic, entry.magic);
        assert_eq!(found.framing, entry.framing);
        assert_eq!(found.protocol, entry.protocol);
        assert_eq!(found.envelope, entry.envelope);
        assert_eq!(found.child_fd, entry.child_fd);
    }
    assert!(
        contract("tilo-fs").is_none(),
        "no handoff for another service"
    );
    assert_eq!(contract(BLOB_SERVICE).unwrap().child_fd, Some(30));
    assert_eq!(BLOB_CHILD_FD, 30);
}

#[test]
fn an_io_error_names_what_the_peer_must_fix() {
    let kind = |error: HandoffError| io::Error::from(error).kind();
    assert_eq!(kind(HandoffError::Missing), io::ErrorKind::UnexpectedEof);
    assert_eq!(kind(HandoffError::Marker), io::ErrorKind::UnexpectedEof);
    assert_eq!(kind(HandoffError::Malformed), io::ErrorKind::InvalidData);
    assert_eq!(
        kind(HandoffError::Protocol {
            found: 3,
            expected: 1
        }),
        io::ErrorKind::InvalidData
    );
}

#[cfg(feature = "tokio")]
#[test]
fn the_async_ends_write_and_read_the_same_bytes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for (contract, handoff) in [
        (&USH, Handoff::app(APP)),
        (&APP_HOST, Handoff::app(APP)),
        (&COLLAB, Handoff::room(APP, ROOM).with_executing_peer()),
        (
            &BLOBS,
            Handoff::Blob(BlobScope::App {
                room: ROOM.into(),
                app_id: APP.into(),
            }),
        ),
    ] {
        runtime.block_on(async {
            let mut buffer = Vec::new();
            contract.write_async(&mut buffer, &handoff).await.unwrap();
            assert_eq!(buffer, contract.encode(&handoff).unwrap());
            let read = contract
                .read_async(&mut buffer.as_slice())
                .await
                .expect("read");
            assert_eq!(read, handoff, "{}", contract.service);
        });
    }
}
