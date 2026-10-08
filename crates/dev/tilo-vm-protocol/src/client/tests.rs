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

#[test]
fn private_device_requests_bypass_the_retained_frame_even_when_the_send_fails() {
    struct FailedWriter;
    impl Write for FailedWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("fixture transport failure"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn check<C: Call>(request: C) {
        let mut client = Client::new(Cursor::new(Vec::<u8>::new()), FailedWriter);
        client.request_buffer = b"retained public upload".to_vec();
        assert!(matches!(client.call(request), Err(ClientError::Io(_))));
        assert_eq!(client.request_buffer, b"retained public upload");
    }
    check(crate::DeviceActions {
        line: r#"{"passphrase":"private-fixture-answer","confirm":true}"#.into(),
    });
    check(crate::DeviceUnlockEnrol {
        passphrase: "private-fixture-answer".into(),
        networks: Vec::new(),
        allow_unsealed: false,
    });
}

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

#[test]
fn serialized_frame_storage_is_reused_without_leaking_previous_request_bytes() {
    let (mut client, wire) = client(Wire::default());
    let upload = client.call(LayerUploadBegin {}).unwrap().upload;
    let chunk = LayerUploadChunk {
        upload,
        data: STANDARD.encode(vec![53; UPLOAD_CHUNK_BYTES]),
    };
    assert_eq!(
        client.call(chunk.clone()).unwrap().received,
        UPLOAD_CHUNK_BYTES as u64
    );
    let capacity = client.request_buffer.capacity();
    let expected = RequestEnvelope {
        jsonrpc: "2.0".into(),
        id: 3,
        request: chunk.clone().into(),
    };
    assert_eq!(
        client.call(chunk).unwrap().received,
        (UPLOAD_CHUNK_BYTES * 2) as u64
    );
    let mut expected_bytes = serde_json::to_vec(&expected).unwrap();
    expected_bytes.push(b'\n');
    assert_eq!(client.request_buffer, expected_bytes);
    assert_eq!(client.request_buffer.capacity(), capacity);
    // The wire decoder parses the complete frame: stale large-request bytes
    // after this tiny request would fail before it could return LayerList.
    assert!(client.call(LayerList {}).unwrap().is_empty());
    assert_eq!(client.request_buffer.capacity(), capacity);
    assert!(client.request_buffer.len() < 128);
    assert_eq!(wire.borrow().batches, [1; 4]);
}

#[test]
fn unusually_large_rpc_frames_are_sent_but_not_retained() {
    let responses = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"received\":0}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":[]}\n"
    );
    let mut client = Client::new(Cursor::new(responses), Vec::new());
    // Direct RPC bodies can be larger than the chunks produced by upload().
    // This only tests framing/cache retention; the daemon owns validation.
    let data = "a".repeat(MAX_RETAINED_REQUEST_BYTES + 1);
    client
        .call(LayerUploadChunk {
            upload: "large-frame".into(),
            data: data.clone(),
        })
        .unwrap();
    assert_eq!(client.request_buffer.capacity(), 0);
    assert!(client.call(LayerList {}).unwrap().is_empty());
    let frames = std::str::from_utf8(&client.writer).unwrap();
    let mut frames = frames.lines();
    let first: RequestEnvelope = serde_json::from_str(frames.next().unwrap()).unwrap();
    let Request::LayerUploadChunk(first) = first.request else {
        panic!("the original request must be sent intact");
    };
    assert_eq!(first.data, data);
    let second: RequestEnvelope = serde_json::from_str(frames.next().unwrap()).unwrap();
    assert!(matches!(second.request, Request::LayerList(_)));
    assert!(frames.next().is_none());
}
