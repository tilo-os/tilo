use super::*;

// Independent construction of the frozen TLD3 header and Postcard upload fields.
fn frame(kind: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = b"TLD3\x03\x00".to_vec();
    bytes.extend_from_slice(&kind.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}
fn upload(id: u8, bytes: &[u8], offset: u64, total: u64, digest: Digest) -> Vec<u8> {
    frame(
        10,
        &postcard::to_stdvec(&(1_u8, [id; 16], digest, total, offset, bytes)).unwrap(),
    )
}
fn complete(id: u8, bytes: &[u8]) -> Vec<u8> {
    upload(
        id,
        bytes,
        0,
        bytes.len() as u64,
        *blake3::hash(bytes).as_bytes(),
    )
}
#[test]
fn complete_batch_borrows_original_bytes_and_preflights_every_item() {
    let mut body = complete(1, b"first native envelope");
    body.extend(complete(2, b"second native envelope"));
    let request = Request::new("POST", "/peer/v3/state", &body).unwrap();
    let items = prepare_state(&request).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].delivery, [1; 16]);
    assert_eq!(items[1].canonical, b"second native envelope");
    assert_eq!(
        items[1].digest,
        *blake3::hash(b"second native envelope").as_bytes()
    );
    let buffer = body.as_ptr_range();
    for item in &items {
        assert!(buffer.contains(&item.canonical.as_ptr()));
    }
    drop(items);
    body.extend_from_slice(b"TLD3");
    assert!(prepare_state(&Request::new("POST", "/peer/v3/state", &body).unwrap()).is_err());
}
#[test]
fn partial_wrong_digest_or_wrong_lane_never_becomes_complete_upload() {
    let hash = *blake3::hash(b"abc").as_bytes();
    for body in [
        upload(1, b"abc", 1, 4, hash),
        upload(1, b"abc", 0, 4, hash),
        upload(1, b"abc", 0, 3, [5; 32]),
        upload(0, b"abc", 0, 3, hash),
        frame(15, b"ack is not state"),
        vec![],
    ] {
        assert!(prepare_state(&Request::new("POST", "/peer/v3/state", &body).unwrap()).is_err());
    }
    let body = complete(1, b"abc");
    for (method, target) in [
        ("GET", "/peer/v3/state"),
        ("POST", "/peer/v3/acks"),
        ("POST", "/peer/v3/state?x=1"),
    ] {
        assert!(prepare_state(&Request::new(method, target, &body).unwrap()).is_err());
    }
}
#[test]
fn upload_batch_has_finite_item_limit_before_native_admission() {
    let mut body = Vec::new();
    for id in 1..=64 {
        body.extend(complete(id, b"a"));
    }
    assert_eq!(
        prepare_state(&Request::new("POST", "/peer/v3/state", &body).unwrap())
            .unwrap()
            .len(),
        64
    );
    body.extend(complete(65, b"a"));
    assert!(prepare_state(&Request::new("POST", "/peer/v3/state", &body).unwrap()).is_err());
}
#[test]
fn ack_requires_one_framed_cursor_and_poll_keeps_token_opaque() {
    let token = CursorToken::from_bytes(&[0, 37, 255]).unwrap();
    let body = frame(15, &CursorAck::new(token).encode().unwrap());
    let ack = prepare_ack(&Request::new("POST", "/peer/v3/acks", &body).unwrap()).unwrap();
    assert_eq!(ack.cursor().as_bytes(), &[0, 37, 255]);
    let mut extra = body.clone();
    extra.extend(&body);
    assert!(prepare_ack(&Request::new("POST", "/peer/v3/acks", &extra).unwrap()).is_err());
    let poll = Request::new("GET", "/peer/v3/events?cursor=0025ff&mode=poll", b"").unwrap();
    assert_eq!(
        prepare_poll(&poll).unwrap().unwrap().as_bytes(),
        &[0, 37, 255]
    );
    assert!(
        prepare_poll(&Request::new("GET", "/peer/v3/events?mode=poll", b"").unwrap())
            .unwrap()
            .is_none()
    );
    for target in [
        "/peer/v3/events?cursor=&mode=poll",
        "/peer/v3/events?cursor=FF&mode=poll",
        "/peer/v3/events?cursor=f&mode=poll",
        "/peer/v3/events?cursor=gg&mode=poll",
        "/peer/v3/events?mode=stream",
    ] {
        assert!(prepare_poll(&Request::new("GET", target, b"").unwrap()).is_err());
    }
    assert!(
        prepare_poll(&Request::new("GET", "/peer/v3/events?mode=poll", b"unexpected").unwrap())
            .is_err()
    );
}

#[test]
fn unsupported_frame_version_is_distinct_from_malformed_input() {
    let mut body = complete(1, b"abc");
    body[4] = 4;
    let request = Request::new("POST", "/peer/v3/state", &body).unwrap();
    assert_eq!(prepare_state(&request).unwrap_err(), DataError::Unsupported);
}
