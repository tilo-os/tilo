use super::*;

fn retained(
    workspace: u64,
    author: u8,
    channel: u8,
    checkpoint: bool,
    body_bytes: usize,
) -> EnvelopeV2 {
    let payload = if checkpoint {
        crate::coedit::CoeditPayload::Checkpoint {
            channel: [channel; 32],
            body: vec![5; body_bytes],
        }
    } else {
        crate::coedit::CoeditPayload::RetainedDelta {
            channel: [channel; 32],
            body: vec![5; body_bytes],
        }
    };
    EnvelopeV2::State(Envelope::Delta {
        workspace_id: workspace,
        author_device_public_key: [author; 32],
        authored_at_ns: 1,
        delta: payload.encode().unwrap(),
        signature: [0; 64],
    })
}

#[test]
fn buffered_prefix_reports_bounded_payload_free_shape() {
    let frames = (0..25)
        .map(|channel| retained(7, 1, channel, true, 3))
        .collect::<Vec<_>>();
    let shape = buffered_retention_prefix(frames.iter()).unwrap();
    assert_eq!(shape.frames, 25);
    assert_eq!(shape.checkpoints, 25);
    assert_eq!(shape.deltas, 0);
    assert_eq!(shape.distinct_channels, 25);
    assert_eq!(shape.stop_reason, "end");
    assert!(!shape.frame_cap_reached);
    assert!(!shape.byte_cap_reached);
    assert!(shape.input_bytes > 25 * 256);
}

#[test]
fn buffered_prefix_stops_at_identity_kind_and_resource_bounds() {
    for (barrier, expected) in [
        (retained(8, 1, 2, true, 3), "workspace_change"),
        (retained(7, 2, 2, true, 3), "author_change"),
        (EnvelopeV2::State(Envelope::Ping), "non_delta"),
        (
            EnvelopeV2::State(Envelope::Delta {
                workspace_id: 7,
                author_device_public_key: [1; 32],
                authored_at_ns: 1,
                delta: vec![9],
                signature: [0; 64],
            }),
            "non_retained",
        ),
    ] {
        let frames = [retained(7, 1, 1, false, 3), barrier];
        let shape = buffered_retention_prefix(frames.iter()).unwrap();
        assert_eq!(shape.frames, 1);
        assert_eq!(shape.checkpoints, 0);
        assert_eq!(shape.deltas, 1);
        assert_eq!(shape.stop_reason, expected);
    }

    for (first, expected) in [
        (EnvelopeV2::State(Envelope::Ping), "non_delta"),
        (
            EnvelopeV2::State(Envelope::Delta {
                workspace_id: 7,
                author_device_public_key: [1; 32],
                authored_at_ns: 1,
                delta: vec![9],
                signature: [0; 64],
            }),
            "non_retained",
        ),
    ] {
        let frames = [first];
        let shape = buffered_retention_prefix(frames.iter()).unwrap();
        assert_eq!(shape.frames, 0);
        assert_eq!(shape.stop_reason, expected);
    }

    let frames = [
        retained(7, 1, 1, true, 3),
        retained(7, 1, 2, true, 3),
        retained(7, 1, 3, true, 3),
    ];
    let frame_limited =
        buffered_retention_prefix_with_limits(frames.iter(), 2, usize::MAX).unwrap();
    assert_eq!(frame_limited.frames, 2);
    assert!(frame_limited.frame_cap_reached);
    assert_eq!(frame_limited.stop_reason, "frame_cap");

    let full_backlog = (0..RETENTION_PREFIX_FRAME_CAP)
        .map(|channel| retained(7, 1, u8::try_from(channel).unwrap(), true, 3))
        .collect::<Vec<_>>();
    let exact_cap = buffered_retention_prefix(full_backlog.iter()).unwrap();
    assert_eq!(exact_cap.frames, RETENTION_PREFIX_FRAME_CAP);
    assert!(exact_cap.frame_cap_reached);
    assert_eq!(exact_cap.stop_reason, "frame_cap");

    let first_bytes = inbound_bytes(&frames[0]);
    let byte_limited =
        buffered_retention_prefix_with_limits(frames.iter(), 32, first_bytes).unwrap();
    assert_eq!(byte_limited.frames, 1);
    assert!(byte_limited.byte_cap_reached);
    assert_eq!(byte_limited.stop_reason, "byte_cap");

    let first_too_large =
        buffered_retention_prefix_with_limits(frames.iter(), 32, first_bytes - 1).unwrap();
    assert_eq!(first_too_large.frames, 0);
    assert!(first_too_large.byte_cap_reached);
    assert_eq!(first_too_large.stop_reason, "byte_cap");
}

#[test]
fn eligible_checkpoint_prefix_requires_a_coherent_distinct_checkpoint_run() {
    assert_eq!(eligible_checkpoint_prefix(&Envelope::Ping, [].iter()), 1);
    let EnvelopeV2::State(current) = retained(7, 1, 1, true, 3) else {
        unreachable!()
    };
    let suffix = [
        retained(7, 1, 2, true, 3),
        retained(7, 1, 2, true, 3),
        retained(7, 1, 3, false, 3),
        retained(7, 1, 4, true, 3),
    ];
    let suffix_before = suffix
        .iter()
        .map(|frame| tilo_delivery_protocol::transport_v2::encode(frame).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(eligible_checkpoint_prefix(&current, suffix.iter()), 2);
    let suffix_after = suffix
        .iter()
        .map(|frame| tilo_delivery_protocol::transport_v2::encode(frame).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(suffix_before, suffix_after);
}

#[test]
fn checkpoint_batch_mode_requires_an_exact_completed_projection_shape() {
    use crate::store::shell_selection::{
        BatchProjectionReason, BatchSelectionOutcome, IncomingDisposition, Publication,
    };
    let represented = BatchSelectionOutcome {
        supplemental_recovery: None,
        publication: Publication::Suppress,
        dispositions: vec![IncomingDisposition::Covered; 2],
        reason: BatchProjectionReason::Represented,
    };
    assert_eq!(
        checkpoint_mode_for(Some(&represented), 2),
        CheckpointBatchMode::Represented
    );
    let all_original = BatchSelectionOutcome {
        supplemental_recovery: None,
        publication: Publication::Original,
        dispositions: vec![IncomingDisposition::Original; 2],
        reason: BatchProjectionReason::FinalBasisOmission,
    };
    assert_eq!(
        checkpoint_mode_for(Some(&all_original), 2),
        CheckpointBatchMode::AllOriginal
    );
    for rejected in [
        BatchSelectionOutcome {
            supplemental_recovery: None,
            publication: Publication::Original,
            dispositions: vec![IncomingDisposition::Original],
            reason: BatchProjectionReason::FinalBasisOmission,
        },
        BatchSelectionOutcome {
            supplemental_recovery: None,
            publication: Publication::Original,
            dispositions: vec![IncomingDisposition::Covered, IncomingDisposition::Original],
            reason: BatchProjectionReason::FinalBasisOmission,
        },
        BatchSelectionOutcome {
            supplemental_recovery: None,
            publication: Publication::Activate(std::path::PathBuf::from("selected")),
            dispositions: vec![IncomingDisposition::Original; 2],
            reason: BatchProjectionReason::FinalBasisOmission,
        },
        BatchSelectionOutcome {
            supplemental_recovery: None,
            publication: Publication::Original,
            dispositions: vec![IncomingDisposition::Original; 2],
            reason: BatchProjectionReason::UnresolvedTailDependencies,
        },
    ] {
        assert_eq!(
            checkpoint_mode_for(Some(&rejected), 2),
            CheckpointBatchMode::Serial
        );
    }
    assert_eq!(checkpoint_mode_for(None, 2), CheckpointBatchMode::Serial);
}

#[test]
fn eligible_checkpoint_prefix_stops_at_every_identity_kind_and_malformed_barrier() {
    let EnvelopeV2::State(current) = retained(7, 1, 1, true, 3) else {
        unreachable!()
    };
    for barrier in [
        retained(7, 1, 9, false, 3),
        retained(8, 1, 2, true, 3),
        retained(7, 2, 2, true, 3),
        EnvelopeV2::State(Envelope::Ping),
        EnvelopeV2::State(Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1; 32],
            authored_at_ns: 1,
            delta: vec![9],
            signature: [0; 64],
        }),
    ] {
        assert_eq!(
            eligible_checkpoint_prefix(&current, [retained(7, 1, 2, true, 3), barrier].iter()),
            2
        );
    }
}

#[test]
fn eligible_checkpoint_prefix_includes_current_and_enforces_frame_and_byte_caps() {
    let EnvelopeV2::State(current) = retained(7, 1, 1, true, 3) else {
        unreachable!()
    };
    let full = (2..=34)
        .map(|channel| retained(7, 1, channel, true, 3))
        .collect::<Vec<_>>();
    assert_eq!(eligible_checkpoint_prefix(&current, full.iter()), 32);

    let EnvelopeV2::State(retained_delta) = retained(7, 1, 2, false, 3) else {
        unreachable!()
    };
    assert_eq!(eligible_checkpoint_prefix(&retained_delta, [].iter()), 1);
    let malformed = Envelope::Delta {
        workspace_id: 7,
        author_device_public_key: [1; 32],
        authored_at_ns: 1,
        delta: vec![9],
        signature: [0; 64],
    };
    assert_eq!(eligible_checkpoint_prefix(&malformed, [].iter()), 1);

    let EnvelopeV2::State(large) = retained(7, 1, 2, true, RETENTION_PREFIX_BYTE_CAP) else {
        unreachable!()
    };
    assert_eq!(eligible_checkpoint_prefix(&large, [].iter()), 1);
    assert_eq!(eligible_checkpoint_prefix(&current, [].iter()), 1);
    assert_eq!(
        eligible_checkpoint_prefix(&current, [EnvelopeV2::State(large)].iter()),
        1
    );
}

#[test]
fn completed_publication_labels_are_fixed_and_payload_free() {
    use crate::store::shell_selection::Publication;

    assert_eq!(
        retention_publication_kind(&Publication::Suppress),
        "suppress"
    );
    assert_eq!(
        retention_publication_kind(&Publication::Original),
        "original"
    );
    assert_eq!(
        retention_publication_kind(&Publication::Activate(std::path::PathBuf::from("ignored"))),
        "activate"
    );
}
