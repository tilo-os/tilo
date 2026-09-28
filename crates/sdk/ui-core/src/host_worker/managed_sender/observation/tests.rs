use super::*;
const ID: [u8; 16] = [8; 16];
const UUID: &str = "08080808-0808-0808-0808-080808080808";
fn run(body: &str, uncertain: bool) -> Result<Facts, OutboxFailure> {
    validate(body, &ID, uncertain, false)
}
fn frame(state: &str, fields: &str) -> String {
    format!(r#"{{"state":"{state}","invocation":"{UUID}"{fields}}}"#)
}
#[test]
fn managed_sender_observation_accepts_full_ordinary_grammar_without_payload_tree() {
    for (state, fields, lifecycle, sequence, consumable) in [
        ("accepted", "", Lifecycle::Accepted, None, false),
        (
            "progress",
            r#", "sequence":0,"phase":"working\nnow","completed":null,"total":20,"message":"quote\"""#,
            Lifecycle::Progress,
            Some(0),
            false,
        ),
        (
            "item",
            r#", "sequence":1,"value":{"arbitrary":[1,null,{"x":true}]}"#,
            Lifecycle::Item,
            Some(1),
            false,
        ),
        (
            "succeeded",
            r#", "output":{"value":{"x":[false,null]},"resources":["tilo.content:abc"]}"#,
            Lifecycle::Succeeded,
            None,
            true,
        ),
        (
            "failed",
            r#", "error":{"code":"ordinary","message":"oops","details":{"x":1},"retryable":true}"#,
            Lifecycle::Failed,
            None,
            true,
        ),
        (
            "cancelled",
            r#", "message":null"#,
            Lifecycle::Cancelled,
            None,
            true,
        ),
    ] {
        assert_eq!(
            run(&frame(state, fields), false).unwrap(),
            Facts {
                lifecycle,
                sequence,
                consumable
            }
        );
    }
}
#[test]
fn managed_sender_observation_rejects_shape_duplicates_correlation_and_false_certainty() {
    for body in [
        frame("accepted", r#", "sequence":1"#),
        frame("accepted", r#", "state":"succeeded"#),
        frame("succeeded", r#", "output":{"resources":{},"unknown":1}"#),
        frame(
            "failed",
            r#", "error":{"code":"x","message":"y","code":"z"}"#,
        ),
        frame("item", r#", "sequence":-1,"value":{}"#),
        frame("progress", r#", "sequence":1"#),
        frame("accepted", "").replace(UUID, "00000000-0000-0000-0000-000000000001"),
        format!("{} true", frame("accepted", "")),
    ] {
        assert!(run(&body, false).is_err(), "{body}");
    }
    assert!(run(&frame("succeeded", r#", "output":{}"#), true).is_err());
}
#[test]
fn managed_sender_observation_unknown_and_sanitized_payload_never_consumable() {
    let unknown = frame(
        "failed",
        r#", "error":{"code":"action_outcome_unknown","message":"unknown","retryable":false}"#,
    );
    assert!(!run(&unknown, true).unwrap().consumable);
    assert!(run(&unknown, false).is_err());
    let sanitized = frame(
        "failed",
        r#", "error":{"code":"action_read_payload_unavailable","message":"unavailable","retryable":false}"#,
    );
    assert!(!run(&sanitized, false).unwrap().consumable);
    let oversized = frame(
        "accepted",
        &format!(
            r#", "x":"{}""#,
            "y".repeat(super::super::codec::MAX_OBSERVATION)
        ),
    );
    assert!(matches!(
        run(&oversized, false),
        Err(OutboxFailure::Capacity)
    ));
}

#[test]
fn managed_sender_observation_streaming_scratch_is_released() {
    use crate::semantic_document_read::retirement::measure_test_allocations;
    let escaped = "\\n".repeat(100_000);
    let success = frame(
        "succeeded",
        &format!(r#", "output":{{"value":"{escaped}"}}"#),
    );
    let unknown_key = format!(r#"{{"{escaped}":0}}"#);
    let malformed_type = frame("item", &format!(r#", "sequence":"{escaped}","value":0"#));
    for (json, valid) in [
        (&success, true),
        (&unknown_key, false),
        (&malformed_type, false),
    ] {
        let (result, allocations) = measure_test_allocations(|| validate(json, &ID, false, false));
        assert_eq!(result.is_ok(), valid);
        assert!(
            allocations.allocations > 0,
            "actual escaped string/error allocation required"
        );
        assert_eq!(allocations.live, 0, "no retained parse backing");
    }
    // Value strings require the same valid Unicode grammar as a real Value
    // decode; IgnoredAny would incorrectly accept this isolated surrogate.
    assert!(run(&frame("item", r#", "sequence":1,"value":"\ud800""#), false).is_err());
}
