use super::*;
#[test]
fn bounded_protocol_refuses_extra_fields_and_oversized_lines() {
    let (tx, rx) = mpsc::sync_channel(8);
    lines(
        io::Cursor::new(b"{\"op\":\"start\",\"source\":\"invented\"}\n".to_vec()),
        tx,
    );
    assert!(rx.recv().unwrap().is_err());
    let (tx, rx) = mpsc::sync_channel(8);
    lines(io::Cursor::new(vec![b'x'; LIMIT + 1]), tx);
    assert!(rx.recv().unwrap().is_err());
}
