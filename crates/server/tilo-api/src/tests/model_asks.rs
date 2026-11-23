//! Server-sent events read by the model ask client (`POST …/model/ask`).

use crate::client::model_asks::Events;
use serde_json::{Value, json};

impl Events {
    /// The events `piece` completes, each its kind and its data as JSON.
    pub(crate) fn read(&mut self, piece: &[u8]) -> Vec<(String, Value)> {
        let mut read = Vec::new();
        self.push(piece, |kind, data| {
            read.push((kind.to_owned(), data));
            None::<()>
        });
        read
    }
}

#[test]
fn a_crlf_stream_gives_one_event() {
    let mut events = Events::default();
    let read = events.read(b"event: text\r\ndata: {\"ok\":true}\r\n\r\n");
    assert_eq!(read, [("text".to_owned(), json!({"ok": true}))]);

    let unnamed = events.read(b"data: {\"ok\":true}\r\n\r\n");
    assert_eq!(unnamed, [("message".to_owned(), json!({"ok": true}))]);
}

#[test]
fn a_cr_stream_gives_one_event() {
    let mut events = Events::default();
    let read = events.read(b"event: text\rdata: {\"ok\":true}\r\r");
    assert_eq!(read, [("text".to_owned(), json!({"ok": true}))]);
}

#[test]
fn a_stream_cut_in_the_middle_of_a_line_end_gives_one_event() {
    let mut events = Events::default();
    let mut collected: Vec<(String, Value)> = Vec::new();
    for piece in [
        &b"event: text\r"[..],
        &b"\ndata: {\"ok\":true}\r"[..],
        &b"\n\r"[..],
        &b"\n"[..],
    ] {
        collected.extend(events.read(piece));
    }
    assert_eq!(collected, [("text".to_owned(), json!({"ok": true}))]);

    let stream = b"event: text\r\ndata: {\"ok\":true}\r\n\r\n";
    for cut in 1..stream.len() {
        let mut reader = Events::default();
        let mut got = Vec::new();
        for piece in stream.chunks(cut) {
            got.extend(reader.read(piece));
        }
        assert_eq!(
            got,
            [("text".to_owned(), json!({"ok": true}))],
            "cut at {cut}"
        );
    }
}

#[test]
fn two_data_lines_join_with_a_newline() {
    // Two `data:` lines joined with a newline produce an unescaped LF inside
    // the JSON string, which is invalid JSON; gluing them without a newline
    // would wrongly parse as `{"text":"ab"}`.
    let mut events = Events::default();
    let glued_only = events.read(b"event: text\ndata: {\"text\":\"a\ndata: b\"}\n\n");
    assert!(
        glued_only.is_empty(),
        "two data lines must join with a newline, not glue together: {glued_only:?}"
    );

    // Valid multi-line JSON across two `data:` lines parses cleanly.
    let multiline = events.read(b"event: text\r\ndata: {\"text\":\r\ndata: \"two lines\"}\r\n\r\n");
    assert_eq!(
        multiline,
        [("text".to_owned(), json!({"text": "two lines"}))]
    );
}
