#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    reason = "in a test these operations form assertions over bounded fixtures"
)]

use runtime_connected::{
    BeginError, BeginFailure, ClientCore, ExchangeError, FeedResponse, TerminalOutcome,
};
use tilo_connected_protocol::{DecodeStatus, Envelope, EnvelopeKind, FrameDecoder, encode_frame};

fn response(correlation: u64, message_type: &str, payload: &str) -> Vec<u8> {
    let envelope =
        Envelope::response(correlation, message_type, payload).expect("response fixture");
    encode_frame(&envelope).expect("response frame")
}

fn literal_frame(body: &[u8]) -> Vec<u8> {
    let length = u32::try_from(body.len()).expect("fixture body fits u32");
    let mut frame = length.to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}

#[test]
fn an_exchange_exposes_one_request_and_completes_on_its_matching_response() {
    // Begin exposes one request, and busy does not replace it.
    let mut core = ClientCore::new();
    let first = core
        .begin("fixture.echo", r#"{"value":1}"#)
        .expect("first request");
    assert_eq!(first.correlation(), 1);
    let mut decoder = FrameDecoder::new();
    let DecodeStatus::Complete { envelope, consumed } =
        decoder.push(first.frame()).expect("outbound frame")
    else {
        panic!("outbound request remained pending");
    };
    assert_eq!(consumed, first.frame().len());
    assert_eq!(envelope.kind(), EnvelopeKind::Request);
    assert_eq!(envelope.correlation(), 1);
    assert_eq!(envelope.message_type(), "fixture.echo");
    assert_eq!(envelope.payload_json(), r#"{"value":1}"#);
    assert_eq!(
        core.begin("fixture.other", "{}"),
        Err(BeginError::DefinitelyNotSent(BeginFailure::Busy))
    );
    let completed = core
        .feed_response(&response(1, "fixture.echo", r#"{"value":1}"#))
        .expect("original exchange remains active");
    assert!(matches!(completed, FeedResponse::Complete(_)));

    // A local validation failure is definitely not sent.
    let mut core = ClientCore::new();
    assert!(matches!(
        core.begin("Bad Type", "{}"),
        Err(BeginError::DefinitelyNotSent(BeginFailure::InvalidRequest(
            _
        )))
    ));
    assert_eq!(core.next_correlation(), Some(1));

    // A fragmented matching response restores ready and preserves the
    // unvalidated payload.
    let mut core = ClientCore::new();
    core.begin("fixture.echo", "{}").expect("request");
    let frame = response(1, "fixture.echo", r#"{"approved":true,"success":true}"#);
    for byte in &frame[..frame.len() - 1] {
        assert_eq!(
            core.feed_response(std::slice::from_ref(byte)),
            Ok(FeedResponse::Pending)
        );
    }
    let FeedResponse::Complete(reply) = core
        .feed_response(&frame[frame.len() - 1..])
        .expect("matching response")
    else {
        panic!("response did not complete");
    };
    assert_eq!(reply.envelope().kind(), EnvelopeKind::Response);
    assert_eq!(
        reply.envelope().payload_json(),
        r#"{"approved":true,"success":true}"#
    );
    let next = core.begin("fixture.echo", "{}").expect("next exchange");
    assert_eq!(next.correlation(), 2);

    // An empty active feed is no progress and not end of input.
    let mut core = ClientCore::new();
    core.begin("fixture.echo", "{}").expect("request");
    assert_eq!(core.feed_response(&[]), Ok(FeedResponse::Pending));
    assert!(!core.is_poisoned());
}

#[test]
fn a_violating_response_poisons_and_retains_the_exposed_outcome() {
    let unknown = |correlation| TerminalOutcome::OutcomeUnknown {
        correlation,
        message_type: "fixture.echo".to_owned(),
    };
    let mut extra_byte = response(1, "fixture.echo", "{}");
    extra_byte.push(0);
    let mut second_frame = response(1, "fixture.echo", "{}");
    second_frame.extend_from_slice(&response(1, "fixture.echo", "{}"));
    let positional =
        literal_frame(br#"["tilo.connected",1,"response","0000000000000001","fixture.echo",{}]"#);
    // Every violation after exposure retains the unknown outcome: later calls
    // are unusable and cancel still reports it.
    let cases = [
        // A matching frame plus any extra byte is rejected atomically; the
        // protocol failure after exposure retains the unknown outcome.
        ("matching frame plus an extra byte", extra_byte),
        // A matching frame plus a second complete frame is unsolicited.
        ("matching frame plus a second frame", second_frame),
        ("positional array response", positional),
    ];
    for (case, chunk) in cases {
        let mut core = ClientCore::new();
        let request = core.begin("fixture.echo", "{}").expect("request");
        assert_eq!(
            core.feed_response(&chunk),
            Err(ExchangeError::ProtocolViolation {
                outcome: unknown(request.correlation()),
            }),
            "case {case}"
        );
        assert!(core.is_poisoned(), "case {case}");
        assert_eq!(
            core.feed_response(&[]),
            Err(ExchangeError::Unusable),
            "case {case}"
        );
        assert_eq!(
            core.begin("fixture.echo", "{}"),
            Err(BeginError::Unusable),
            "case {case}"
        );
        assert_eq!(core.cancel(), unknown(request.correlation()), "case {case}");
    }

    // Unsolicited data while ready has no operation outcome.
    let mut core = ClientCore::new();
    assert_eq!(
        core.feed_response(&response(1, "fixture.echo", "{}")),
        Err(ExchangeError::ProtocolViolation {
            outcome: TerminalOutcome::NoOperation,
        })
    );
    assert!(core.is_poisoned());
    assert_eq!(core.cancel(), TerminalOutcome::Unusable);

    // A malformed or mismatched response poisons without yielding a frame.
    let cases = [
        response(2, "fixture.echo", "{}"),
        response(1, "fixture.other", "{}"),
        encode_frame(&Envelope::request(1, "fixture.echo", "{}").expect("request envelope"))
            .expect("request frame"),
        vec![0, 0, 0, 1, b'{'],
        vec![0, 4, 0, 1],
    ];
    for bytes in cases {
        let mut core = ClientCore::new();
        core.begin("fixture.echo", "{}").expect("request");
        assert!(core.feed_response(&bytes).is_err(), "case {bytes:?}");
        assert!(core.is_poisoned(), "case {bytes:?}");
    }

    // Replaying an old response against the next exchange poisons.
    let mut core = ClientCore::new();
    core.begin("fixture.echo", "{}").expect("request one");
    let old = response(1, "fixture.echo", "{}");
    assert!(matches!(
        core.feed_response(&old),
        Ok(FeedResponse::Complete(_))
    ));
    core.begin("fixture.echo", "{}").expect("request two");
    assert_eq!(
        core.feed_response(&old),
        Err(ExchangeError::ProtocolViolation {
            outcome: unknown(2),
        })
    );
    assert!(core.is_poisoned());
}

#[test]
fn terminal_events_poison_and_report_only_what_was_exposed() {
    // phase: terminal_events_after_byte_exposure_make_the_outcome_unknown
    {
        for terminate in [
            ClientCore::cancel as fn(&mut ClientCore) -> TerminalOutcome,
            ClientCore::transport_failed,
            ClientCore::end_of_input,
        ] {
            let mut core = ClientCore::new();
            let request = core.begin("fixture.echo", "{}").expect("request");
            let outcome = terminate(&mut core);
            assert_eq!(
                outcome,
                TerminalOutcome::OutcomeUnknown {
                    correlation: request.correlation(),
                    message_type: "fixture.echo".to_owned(),
                }
            );
            assert!(core.is_poisoned());
        }
    }
    // phase: truncated_response_is_pending_until_terminal_notification
    {
        let full = response(1, "fixture.echo", "{}");
        for partial_length in [1, 4, full.len() - 1] {
            let mut core = ClientCore::new();
            core.begin("fixture.echo", "{}").expect("request");
            assert_eq!(
                core.feed_response(&full[..partial_length]),
                Ok(FeedResponse::Pending)
            );
            assert!(matches!(
                core.end_of_input(),
                TerminalOutcome::OutcomeUnknown { correlation: 1, .. }
            ));
        }
    }
    // phase: terminal_events_while_ready_poison_without_an_operation_outcome
    {
        for terminate in [
            ClientCore::cancel as fn(&mut ClientCore) -> TerminalOutcome,
            ClientCore::transport_failed,
            ClientCore::end_of_input,
        ] {
            let mut core = ClientCore::new();
            assert_eq!(terminate(&mut core), TerminalOutcome::NoOperation);
            assert!(core.is_poisoned());
            assert_eq!(core.begin("fixture.echo", "{}"), Err(BeginError::Unusable));
            assert_eq!(core.feed_response(&[]), Err(ExchangeError::Unusable));
        }
    }
    // phase: late_response_and_every_call_after_poison_are_unusable
    {
        let mut core = ClientCore::new();
        core.begin("fixture.echo", "{}").expect("request");
        assert!(matches!(
            core.cancel(),
            TerminalOutcome::OutcomeUnknown { .. }
        ));

        assert_eq!(
            core.feed_response(&response(1, "fixture.echo", "{}")),
            Err(ExchangeError::Unusable)
        );
        assert_eq!(core.feed_response(&[]), Err(ExchangeError::Unusable));
        assert_eq!(core.begin("fixture.echo", "{}"), Err(BeginError::Unusable));
    }
}
