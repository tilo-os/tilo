use super::*;

fn collect(event: &Response, frames: &mut Vec<(String, serde_json::Value)>) -> bool {
    frames.push((
        event.event.clone().unwrap(),
        serde_json::from_str(event.output.as_deref().unwrap()).unwrap(),
    ));
    true
}

#[test]
fn coalescing_uses_time_size_and_utf8_boundaries_then_flushes_before_authorization() {
    let base = Instant::now();
    let big = "🌊".repeat(75);
    let pieces = ["{\"re", "ply", "\":\"h", "i", &big, "\"}"];
    let mut calls = vec![StreamCall {
        id: "call_p".into(),
        name: "plan".into(),
        arguments: String::new(),
    }];
    let mut coalescer = DeltaCoalescer::default();
    let mut frames = Vec::new();
    for (piece, ms) in pieces.iter().zip([0, 10, 20, 45, 50, 60]) {
        calls[0].arguments.push_str(piece);
        coalescer
            .flush(
                &calls,
                0,
                base + Duration::from_millis(ms),
                false,
                &mut |event| collect(event, &mut frames),
            )
            .unwrap_or_else(|failure| panic!("{}", failure.why));
    }
    let mut timing = PlanTiming::start();
    emit_complete(
        &calls,
        0,
        1,
        &mut |event| collect(event, &mut frames),
        &mut timing,
        Some(&mut coalescer),
    )
    .unwrap_or_else(|failure| panic!("{}", failure.why));
    let deltas = frames
        .iter()
        .filter(|(kind, _)| kind == EVENT_TOOL_CALL_DELTA)
        .map(|(_, value)| value["delta"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(deltas, ["{\"re", "ply\":\"hi", big.as_str(), "\"}"]);
    assert_eq!(frames.last().unwrap().0, EVENT_TOOL_CALL);
    assert_eq!(frames.last().unwrap().1["arguments"], deltas.concat());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&deltas.concat()).unwrap()["reply"],
        format!("hi{big}")
    );
    for (_, frame) in &frames[..frames.len() - 1] {
        assert_eq!(frame["index"], 0);
        assert_eq!(frame["id"], "call_p");
        assert_eq!(frame["name"], "plan");
    }
}

#[test]
fn capped_presentation_deltas_do_not_truncate_authoritative_arguments() {
    let mut calls = vec![StreamCall {
        id: String::new(),
        name: "plan".into(),
        arguments: "{\"text\":\"".into(),
    }];
    let mut coalescer = DeltaCoalescer::default();
    let base = Instant::now();
    let mut frames = Vec::new();
    for index in 0..MAX_DELTA_EVENTS + 100 {
        calls[0].arguments.push('x');
        coalescer
            .flush(
                &calls,
                0,
                base + Duration::from_millis(index as u64 * 50),
                false,
                &mut |event| collect(event, &mut frames),
            )
            .unwrap_or_else(|failure| panic!("{}", failure.why));
    }
    calls[0].arguments.push_str("\"}");
    emit_complete(
        &calls,
        0,
        1,
        &mut |event| collect(event, &mut frames),
        &mut PlanTiming::start(),
        Some(&mut coalescer),
    )
    .unwrap_or_else(|failure| panic!("{}", failure.why));
    assert_eq!(frames.len(), MAX_DELTA_EVENTS + 1);
    assert!(frames[0].1["id"].is_null());
    assert_eq!(frames.last().unwrap().0, EVENT_TOOL_CALL);
    let arguments = frames.last().unwrap().1["arguments"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(arguments).unwrap()["text"],
        "x".repeat(MAX_DELTA_EVENTS + 100)
    );
}
