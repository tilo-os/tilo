//! Exercise the production client's wire calls and acknowledgment boundaries.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test setup and assertions"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Cursor};
use std::rc::Rc;

use serde_json::{Value, json};

use super::*;
use crate::{LayerList, Request};

#[derive(Default)]
struct Wire {
    replies: VecDeque<Value>,
    batches: Vec<usize>,
    chunks: usize,
    received: u64,
    failure: Option<&'static str>,
}

struct Reader {
    wire: Rc<RefCell<Wire>>,
    buffered: Cursor<Vec<u8>>,
}

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let bytes = self.fill_buf()?;
        let count = bytes.len().min(out.len());
        out.get_mut(..count)
            .unwrap()
            .copy_from_slice(bytes.get(..count).unwrap());
        self.consume(count);
        Ok(count)
    }
}

impl BufRead for Reader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.buffered.position() == self.buffered.get_ref().len() as u64 {
            let mut wire = self.wire.borrow_mut();
            let count = wire.replies.len();
            if count != 0 {
                wire.batches.push(count);
                let mut bytes = Vec::new();
                for reply in wire.replies.drain(..) {
                    serde_json::to_writer(&mut bytes, &reply).unwrap();
                    bytes.push(b'\n');
                }
                self.buffered = Cursor::new(bytes);
            }
        }
        self.buffered.fill_buf()
    }

    fn consume(&mut self, count: usize) {
        self.buffered.consume(count);
    }
}

struct Writer(Rc<RefCell<Wire>>);

impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let request: RequestEnvelope = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        let mut wire = self.0.borrow_mut();
        let mut id = request.id;
        let mut error = None;
        let result = match request.request {
            Request::LayerUploadBegin(_) => json!({"upload": "test-upload"}),
            Request::LayerUploadChunk(chunk) => {
                let data = STANDARD.decode(chunk.data).map_err(io::Error::other)?;
                if chunk.upload != "test-upload" || data.len() > UPLOAD_CHUNK_BYTES {
                    return Err(io::Error::other("invalid upload chunk"));
                }
                wire.chunks = wire.chunks.saturating_add(1);
                wire.received = wire.received.saturating_add(data.len() as u64);
                let mut received = wire.received;
                if wire.chunks == 2 {
                    match wire.failure {
                        Some("rpc") => {
                            error = Some(json!({
                                "code": 11, "message": "write failed", "hint": "retry upload"
                            }));
                        }
                        Some("count") => received = received.saturating_sub(1),
                        Some("id") => id = id.saturating_add(1),
                        _ => {}
                    }
                }
                json!({"received": received})
            }
            Request::LayerList(_) => json!([]),
            other => return Err(io::Error::other(format!("unexpected request {other:?}"))),
        };
        wire.replies.push_back(match error {
            Some(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
            None => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        });
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn client(wire: Wire) -> (Client<Reader, Writer>, Rc<RefCell<Wire>>) {
    let shared = Rc::new(RefCell::new(wire));
    let client = Client::new(
        Reader {
            wire: shared.clone(),
            buffered: Cursor::new(Vec::new()),
        },
        Writer(shared.clone()),
    );
    (client, shared)
}

#[test]
fn upload_acknowledges_each_chunk_and_preserves_the_tail() {
    let bytes = vec![19; UPLOAD_CHUNK_BYTES * 6 + 7];
    let (mut client, wire) = client(Wire::default());
    assert_eq!(client.upload(bytes.as_slice()).unwrap(), "test-upload");
    assert_eq!(wire.borrow().received, bytes.len() as u64);
    assert_eq!(wire.borrow().batches, [1; 8]);
    assert!(wire.borrow().replies.is_empty());
    assert!(client.call(LayerList {}).unwrap().is_empty());
}

#[test]
fn upload_errors_stop_before_sending_another_chunk() {
    let bytes = vec![7; UPLOAD_CHUNK_BYTES * 5];
    for failure in ["rpc", "count", "id"] {
        let (mut client, wire) = client(Wire {
            failure: Some(failure),
            ..Wire::default()
        });
        let error = client.upload(bytes.as_slice()).unwrap_err();
        match failure {
            "rpc" => assert!(matches!(error, ClientError::Rpc(_))),
            _ => assert!(matches!(error, ClientError::Protocol(_))),
        }
        assert_eq!(wire.borrow().chunks, 2, "stop after the rejected chunk");
        assert!(wire.borrow().replies.is_empty());
        assert!(client.call(LayerList {}).unwrap().is_empty());
    }
    let (mut client, wire) = client(Wire::default());
    client.upload(&b""[..]).unwrap();
    assert_eq!(wire.borrow().chunks, 0);
}

#[test]
fn source_failure_leaves_no_pending_acknowledgment() {
    struct FailingRead(Cursor<Vec<u8>>);
    impl Read for FailingRead {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.0.position() == self.0.get_ref().len() as u64 {
                return Err(io::Error::other("source disappeared"));
            }
            self.0.read(bytes)
        }
    }
    let (mut client, wire) = client(Wire::default());
    let bytes = FailingRead(Cursor::new(vec![17; UPLOAD_CHUNK_BYTES]));
    assert!(matches!(client.upload(bytes), Err(ClientError::Io(_))));
    assert_eq!(wire.borrow().chunks, 1);
    assert_eq!(wire.borrow().batches, [1, 1]);
    assert!(wire.borrow().replies.is_empty());
    assert!(client.call(LayerList {}).unwrap().is_empty());
}
