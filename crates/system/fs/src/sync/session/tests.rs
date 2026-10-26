use super::*;

#[tokio::test]
async fn session_frames_are_big_endian_length_then_postcard() {
    let mut wire = Vec::new();
    SESSION_FRAMING
        .write_value_async(&mut wire, &SessionFrameV1::NamespaceChunk(b"ab".to_vec()))
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protocol_three_namespace_requests_are_refused_by_version() {
    use crate::sync::{Workspace, bind_certified_test_endpoint};

    let (host_endpoint, host_lookup, host_identity) = bind_certified_test_endpoint().await;
    let store = Arc::new(ContentStore::memory());
    let hydrator = Arc::new(Hydrator::new(host_endpoint.clone(), Arc::clone(&store)));
    let (_workspace, ticket) = Workspace::host(
        host_endpoint,
        host_lookup,
        store,
        hydrator,
        Arc::new(Mutex::new(
            crate::namespace::Namespace::new("session-refusal").unwrap(),
        )),
        host_identity,
        None,
    )
    .await
    .unwrap();

    let (client_endpoint, _lookup, client_identity) = bind_certified_test_endpoint().await;
    let connection = client_endpoint
        .connect(ticket.node.clone(), TILO_FS_SESSION_ALPN)
        .await
        .unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    authenticate_connection(
        &client_endpoint,
        &connection,
        &mut recv,
        &mut send,
        &client_identity,
    )
    .await
    .unwrap();
    send.finish().unwrap();
    // Requests ride their own bidirectional stream after the handshake.
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    let request = SessionRequestV1 {
        protocol_version: 3,
        workspace_id: ticket.workspace_id(),
        operation: SessionOperationV1::NamespaceSync,
    };
    SESSION_FRAMING
        .write_value_async(&mut send, &request)
        .await
        .unwrap();
    let frame: SessionFrameV1 = SESSION_FRAMING
        .with_max_len(MAX_FRAME_BYTES)
        .read_value_async(&mut recv)
        .await
        .unwrap()
        .expect("the session answers an old protocol request");
    assert!(matches!(
        frame,
        SessionFrameV1::Error(RecordMeshErrorCodeV1::UnsupportedVersion)
    ));
}
