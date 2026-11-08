use super::*;

#[tokio::test]
async fn session_frames_are_big_endian_length_then_postcard() {
    let mut wire = Vec::new();
    SESSION_FRAMING
        // A variant retired in place keeps its wire position.
        .write_value_async(
            &mut wire,
            &SessionFrameV1::RetiredNamespaceChunk(b"ab".to_vec()),
        )
        .await
        .unwrap();
    assert_eq!(wire, [0, 0, 0, 4, 5, 2, b'a', b'b']);

    let frame: Option<SessionFrameV1> = SESSION_FRAMING
        .with_max_len(MAX_FRAME_BYTES)
        .read_value_async(&mut &[0, 0, 0, 1, 9][..])
        .await
        .unwrap();
    assert!(matches!(frame, Some(SessionFrameV1::Done)));
}

#[tokio::test]
async fn session_reads_are_bounded_by_the_step_limit() {
    let over = u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes();
    assert!(matches!(
        SESSION_FRAMING
            .with_max_len(MAX_FRAME_BYTES)
            .read_value_async::<_, SessionFrameV1>(&mut &over[..])
            .await,
        Err(FrameError::TooLarge(_))
    ));
    // A sync round may carry up to the larger sync bound.
    assert!(matches!(
        SESSION_FRAMING
            .with_max_len(MAX_SYNC_FRAME_BYTES)
            .read_value_async::<_, SessionFrameV1>(&mut &over[..])
            .await,
        Err(FrameError::Truncated)
    ));
    let too_big = u32::try_from(MAX_SYNC_FRAME_BYTES + 1)
        .unwrap()
        .to_be_bytes();
    assert!(matches!(
        SESSION_FRAMING
            .with_max_len(MAX_SYNC_FRAME_BYTES)
            .read_value_async::<_, SessionFrameV1>(&mut &too_big[..])
            .await,
        Err(FrameError::TooLarge(_))
    ));
}
