use crate::{
    Admission, Codec, FrameMeta, InvalidFrame, MAX_CALLSTREAM_PAYLOAD, MAX_MOQ_OBJECT,
    MAX_PICTURE_SIDE, Profile, Sequence, SequenceError, SequenceEvent, UnitChain, validate_payload,
    validate_side,
};

#[test]
fn both_profiles_name_their_own_codecs_and_refuse_the_others() {
    assert_eq!(Profile::Callstream.name(), "tilo-callstream/13");
    assert_eq!(Profile::Moq.name(), "tilo-video/1");
    assert_eq!(
        Profile::Callstream.codecs(),
        &[Codec::H264, Codec::Vp9, Codec::Av1, Codec::HevcMain10]
    );
    assert_eq!(
        Profile::Moq.codecs(),
        &[Codec::H264, Codec::Vp8, Codec::Vp9, Codec::Av1]
    );
    for profile in [Profile::Callstream, Profile::Moq] {
        for codec in profile.codecs() {
            let id = profile
                .codec_id(*codec)
                .unwrap_or_else(|| panic!("{profile:?} names {codec:?}"));
            assert_eq!(
                profile.codec(id),
                Some(*codec),
                "{profile:?} id {id} reads back"
            );
        }
    }
    assert_eq!(Profile::Callstream.codec_id(Codec::H264), Some(0));
    assert_eq!(Profile::Callstream.codec_id(Codec::Vp9), Some(1));
    assert_eq!(Profile::Callstream.codec_id(Codec::Av1), Some(2));
    assert_eq!(Profile::Callstream.codec_id(Codec::HevcMain10), Some(3));
    assert_eq!(Profile::Callstream.codec_id(Codec::Vp8), None);
    assert_eq!(Profile::Callstream.codec(4), None);
    assert_eq!(Profile::Moq.codec_id(Codec::H264), Some(1));
    assert_eq!(Profile::Moq.codec_id(Codec::Vp8), Some(2));
    assert_eq!(Profile::Moq.codec_id(Codec::Vp9), Some(3));
    assert_eq!(Profile::Moq.codec_id(Codec::Av1), Some(4));
    assert_eq!(Profile::Moq.codec_id(Codec::HevcMain10), None);
    assert_eq!(Profile::Moq.codec(0), None);
    for codec in Codec::ALL {
        assert_eq!(Codec::from_label(codec.label()), Some(codec));
    }
    assert_eq!(Codec::from_label("mpeg2"), None);
    assert_eq!(Codec::HevcMain10.bit_depth(), 10);
    assert_eq!(Codec::Vp8.bit_depth(), 8);
    assert!(Codec::Vp9.is_royalty_free() && Codec::Av1.is_royalty_free());
    assert!(!Codec::H264.is_royalty_free());
}

#[test]
fn payload_and_side_bounds_are_refused_at_the_boundary() {
    assert!(validate_payload(0, MAX_CALLSTREAM_PAYLOAD).is_ok());
    assert!(validate_payload(MAX_CALLSTREAM_PAYLOAD, MAX_CALLSTREAM_PAYLOAD).is_ok());
    assert_eq!(
        validate_payload(
            MAX_CALLSTREAM_PAYLOAD.saturating_add(1),
            MAX_CALLSTREAM_PAYLOAD
        ),
        Err(InvalidFrame::PayloadTooLarge {
            len: MAX_CALLSTREAM_PAYLOAD.saturating_add(1),
            max: MAX_CALLSTREAM_PAYLOAD,
        })
    );
    assert!(validate_payload(MAX_MOQ_OBJECT, MAX_MOQ_OBJECT).is_ok());
    assert!(validate_side(MAX_PICTURE_SIDE, MAX_PICTURE_SIDE, MAX_PICTURE_SIDE).is_ok());
    assert_eq!(
        validate_side(0, 180, MAX_PICTURE_SIDE),
        Err(InvalidFrame::EmptySide {
            width: 0,
            height: 180,
        })
    );
    assert_eq!(
        validate_side(MAX_PICTURE_SIDE.saturating_add(1), 180, MAX_PICTURE_SIDE),
        Err(InvalidFrame::SideTooLarge {
            width: MAX_PICTURE_SIDE.saturating_add(1),
            height: 180,
            max: MAX_PICTURE_SIDE,
        })
    );
}

#[test]
fn frame_metadata_carries_time_and_size_only_where_the_profile_does() {
    let callstream = FrameMeta::new(true, Codec::H264, 7);
    assert_eq!((callstream.width(), callstream.height()), (0, 0));
    assert_eq!(callstream.timestamp_us, None);

    let moq = FrameMeta::new(false, Codec::Av1, 9)
        .with_timestamp(1_000_000)
        .with_display_size(640, 360);
    assert_eq!(moq.timestamp_us, Some(1_000_000));
    assert_eq!((moq.width(), moq.height()), (640, 360));
    assert_eq!(
        moq,
        FrameMeta {
            keyframe: false,
            codec: Codec::Av1,
            sequence: 9,
            timestamp_us: Some(1_000_000),
            display_size: Some((640, 360)),
        }
    );
}

#[test]
fn a_gap_holds_deltas_until_a_keyframe_and_a_reconnect_starts_a_new_chain() {
    let mut chain = UnitChain::default();
    assert_eq!(
        chain.admit(0, false),
        Admission::AwaitingKeyframe { broke: false },
        "a stream starts at a keyframe"
    );
    assert_eq!(chain.admit(1, true), Admission::Decode);
    assert_eq!(chain.admit(2, false), Admission::Decode);
    assert_eq!(chain.admit(2, false), Admission::Stale, "a repeat");
    assert_eq!(
        chain.admit(4, false),
        Admission::AwaitingKeyframe { broke: true },
        "unit 3 went missing"
    );
    assert_eq!(
        chain.admit(3, false),
        Admission::AwaitingKeyframe { broke: false },
        "arriving late does not mend the chain"
    );
    assert_eq!(
        chain.admit(5, false),
        Admission::AwaitingKeyframe { broke: false }
    );
    assert_eq!(
        chain.admit(0, true),
        Admission::Decode,
        "a restarted session starts a new chain at its own keyframe"
    );
    assert_eq!(chain.admit(1, false), Admission::Decode);
    chain.lose();
    assert!(!chain.intact());
    assert_eq!(
        chain.admit(2, false),
        Admission::AwaitingKeyframe { broke: false }
    );
    assert_eq!(chain.admit(9, true), Admission::Decode);
    assert!(chain.intact());
}

#[test]
fn a_sequence_reports_gaps_staleness_and_reconnect() {
    let mut sequence = Sequence::default();
    assert_eq!(sequence.observe(5), Ok(SequenceEvent::First));
    assert_eq!(sequence.observe(6), Ok(SequenceEvent::Continuous));
    assert_eq!(
        sequence.observe(9),
        Ok(SequenceEvent::Gap { dropped: 2 }),
        "7 and 8 never arrived"
    );
    assert_eq!(
        sequence.observe(8),
        Err(SequenceError::Stale {
            previous: 9,
            received: 8,
        })
    );
    assert_eq!(sequence.last(), None, "a stale unit forgets the history");
    assert_eq!(sequence.observe(8), Ok(SequenceEvent::First));

    assert_eq!(
        sequence.observe(u64::MAX),
        Ok(SequenceEvent::Gap {
            dropped: u64::MAX.saturating_sub(9),
        })
    );
    assert_eq!(sequence.observe(0), Err(SequenceError::Exhausted));
    assert_eq!(
        sequence.observe(0),
        Ok(SequenceEvent::First),
        "a reconnected stream starts again at zero"
    );
    sequence.reset();
    assert_eq!(sequence.observe(0), Ok(SequenceEvent::First));
}
