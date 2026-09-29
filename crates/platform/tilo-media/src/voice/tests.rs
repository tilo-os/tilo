use super::*;

fn publisher(name: &str, generation: u64) -> PublicationId {
    PublicationId {
        namespace: name.into(),
        carrier_generation: generation,
    }
}
fn received(name: &str, sequence: u64, at: Instant) -> ReceivedOpus {
    ReceivedOpus {
        publisher: publisher(name, 1),
        sequence,
        timestamp_48k: sequence.checked_mul(960).unwrap(),
        duration_samples: 960,
        payload: vec![0xf8, 0xff, 0xfe],
        received_at: at,
        expires_at: at.checked_add(Duration::from_millis(200)).unwrap(),
    }
}
fn captured(sequence: u64, at: Instant) -> CapturedOpus {
    CapturedOpus {
        timestamp_48k: sequence.checked_mul(960).unwrap(),
        duration_samples: 960,
        payload: vec![0xf8, 0xff, 0xfe],
        captured_at: at,
    }
}

#[test]
fn capture_restarts_keep_a_monotonic_timeline_and_preserve_source_gaps() {
    let now = Instant::now();
    let mut timeline = CaptureTimeline::default();
    assert_eq!(timeline.timestamp(648, now).unwrap(), 0);
    assert_eq!(
        timeline
            .timestamp(3528, now + Duration::from_millis(60))
            .unwrap(),
        2880
    );
    timeline.reset_source();
    assert_eq!(
        timeline
            .timestamp(648, now + Duration::from_millis(65))
            .unwrap(),
        3840
    );
    assert_eq!(
        timeline
            .timestamp(1608, now + Duration::from_millis(85))
            .unwrap(),
        4800
    );
}

#[test]
fn receiver_uses_timestamps_for_loss_and_sequence_only_for_object_order() {
    let now = Instant::now();
    let mut queue = JitterBuffer::default();
    queue.push(received("alice", 0, now), now).unwrap();
    let mut gap_in_sequence = received("alice", 5, now);
    gap_in_sequence.timestamp_48k = 960;
    queue.push(gap_in_sequence, now).unwrap();
    let mut gap_in_capture = received("alice", 6, now);
    gap_in_capture.timestamp_48k = 2880;
    queue.push(gap_in_capture, now).unwrap();
    assert!(
        queue.poll(now + Duration::from_millis(60))[0]
            .payload
            .is_some()
    );
    let frame = queue.poll(now + Duration::from_millis(80));
    assert_eq!(frame[0].sequence, 5);
    assert_eq!(frame[0].timestamp_48k, 960);
    assert!(frame[0].payload.is_some());
    let loss = queue.poll(now + Duration::from_millis(100));
    assert_eq!(loss[0].timestamp_48k, 1920);
    assert!(loss[0].payload.is_none());
    let frame = queue.poll(now + Duration::from_millis(120));
    assert_eq!(frame[0].sequence, 6);
    assert!(frame[0].payload.is_some());
}

#[test]
fn capture_is_muted_until_started_and_stop_discards_queued_speech() {
    let now = Instant::now();
    let mut queue = CaptureQueue::default();
    assert_eq!(queue.push(captured(0, now), now), Err(AudioError::Revoked));
    queue.start();
    queue.push(captured(0, now), now).unwrap();
    assert_eq!(queue.drain(now).len(), 1);
    queue.push(captured(1, now), now).unwrap();
    queue.stop();
    queue.start();
    assert!(
        queue.drain(now).is_empty(),
        "old speech cannot survive mute/restart"
    );
}

#[test]
fn capture_holds_five_frames_and_drops_at_original_hundred_ms_deadline() {
    let now = Instant::now();
    let mut queue = CaptureQueue::default();
    queue.start();
    for seq in 0..6 {
        queue.push(captured(seq, now), now).unwrap();
    }
    let frames = queue.drain(now);
    assert_eq!(frames.len(), 5);
    assert_eq!(frames[0].timestamp_48k, 960);
    queue.push(captured(8, now), now).unwrap();
    assert!(queue.drain(now + Duration::from_millis(100)).is_empty());
}

#[test]
fn jitter_orders_frames_and_counts_duplicates_without_mixing_publishers() {
    let now = Instant::now();
    let mut queue = JitterBuffer::default();
    queue.push(received("alice", 0, now), now).unwrap();
    queue.push(received("bob", 10, now), now).unwrap();
    queue.push(received("alice", 2, now), now).unwrap();
    queue.push(received("alice", 1, now), now).unwrap();
    queue.push(received("alice", 1, now), now).unwrap();
    assert_eq!(queue.stats().duplicates, 1);
    assert!(queue.poll(now + Duration::from_millis(39)).is_empty());
    let first = queue.poll(now + Duration::from_millis(60));
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].publisher.namespace, "alice");
    assert_eq!(first[0].sequence, 0);
    assert_eq!(first[1].sequence, 10);
    let second = queue.poll(now + Duration::from_millis(81));
    assert_eq!(second[0].sequence, 1);
    assert!(second[0].payload.is_some());
}

#[test]
fn receiver_preserves_earliest_deadline_and_conceals_missing_frame() {
    let now = Instant::now();
    let mut queue = JitterBuffer::default();
    queue.push(received("alice", 0, now), now).unwrap();
    let mut late = received("alice", 1, now);
    late.expires_at = now + Duration::from_millis(70);
    queue.push(late, now).unwrap();
    assert!(
        queue.poll(now + Duration::from_millis(60))[0]
            .payload
            .is_some()
    );
    let concealed = queue.poll(now + Duration::from_millis(81));
    assert!(concealed[0].payload.is_none());
    assert_eq!(queue.stats().late, 1);
    assert_eq!(queue.stats().lost, 1);
    assert!(
        queue.poll(now + Duration::from_millis(250)).is_empty(),
        "inactive decoder stops"
    );
}

#[test]
fn receiver_capacity_and_invalid_timing_do_not_allocate_unbounded_state() {
    let now = Instant::now();
    let mut queue = JitterBuffer::default();
    for index in 0..8 {
        queue
            .push(received(&format!("p{index}"), 0, now), now)
            .unwrap();
    }
    assert_eq!(
        queue.push(received("ninth", 0, now), now),
        Err(AudioError::Capacity)
    );
    for sequence in 1..10 {
        queue.push(received("p0", sequence, now), now).unwrap();
    }
    assert_eq!(
        queue.push(received("p0", 10, now), now),
        Err(AudioError::Capacity)
    );
    assert_eq!(queue.queued(), 17);
    let mut invalid = received("p0", 1, now);
    invalid.timestamp_48k = 1;
    assert_eq!(queue.push(invalid, now), Err(AudioError::InvalidFrame));
    invalid = received("p0", 1, now);
    invalid.duration_samples = 480;
    assert_eq!(queue.push(invalid, now), Err(AudioError::InvalidFrame));
    invalid = received("p0", 1, now);
    invalid.payload.resize(4065, 0);
    assert_eq!(queue.push(invalid, now), Err(AudioError::InvalidFrame));
}

#[test]
fn reconnect_generation_discards_decoder_queue_and_rejects_old_generation() {
    let now = Instant::now();
    let mut queue = JitterBuffer::default();
    queue.push(received("alice", 9, now), now).unwrap();
    let mut replacement = received("alice", 0, now);
    replacement.publisher.carrier_generation = 2;
    queue.push(replacement, now).unwrap();
    assert_eq!(queue.queued(), 1);
    assert_eq!(
        queue.push(received("alice", 10, now), now),
        Err(AudioError::Revoked)
    );
    let frame = queue.poll(now + Duration::from_millis(60));
    assert_eq!(frame.len(), 1);
    assert_eq!(frame[0].sequence, 0);
    assert_eq!(frame[0].publisher.carrier_generation, 2);
    queue.remove(&publisher("alice", 2));
    assert!(queue.poll(now + Duration::from_millis(80)).is_empty());
}

#[test]
fn jitter_target_adapts_within_bounds_without_extending_packet_lifetime() {
    let start = Instant::now();
    let mut queue = JitterBuffer::default();
    queue.push(received("alice", 0, start), start).unwrap();
    assert_eq!(queue.target_ms(&publisher("alice", 1)), Some(60));
    for seq in 1..80 {
        // Alternate 10/30ms arrival gaps without moving the local clock backwards.
        let now = start + Duration::from_millis(seq * 20 + (seq % 2) * 10);
        let _ = queue.push(received("alice", seq, now), now);
        queue.poll(now);
        let target = queue.target_ms(&publisher("alice", 1)).unwrap();
        assert!((40..=120).contains(&target));
    }
    assert!(queue.target_ms(&publisher("alice", 1)).unwrap() > 60);
}

#[test]
fn mixing_sums_independent_publishers_then_saturates() {
    let positive = vec![30_000; 960];
    let negative = vec![-30_000; 960];
    let quiet = vec![10_000; 960];
    assert_eq!(mix_frames(&[&positive, &negative, &quiet]).unwrap(), quiet);
    assert_eq!(
        mix_frames(&[&positive, &positive]).unwrap(),
        vec![32_767; 960]
    );
    assert_eq!(
        mix_frames(&[&negative, &negative]).unwrap(),
        vec![-32_768; 960]
    );
    assert_eq!(mix_frames(&[&[1, 2]]), Err(AudioError::InvalidFrame));
    assert_eq!(
        mix_frames(&[positive.as_slice(); 9]),
        Err(AudioError::Capacity)
    );
}
