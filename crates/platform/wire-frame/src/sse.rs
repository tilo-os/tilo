//! Incremental framing for `text/event-stream` (Server-Sent Events) byte streams.
//!
//! One parser for every SSE reader in the tree:
//! - line endings are LF (`\n`), CR (`\r`) or CRLF (`\r\n`), cut at any byte boundary;
//! - comment lines starting with `:` are ignored;
//! - at most one leading space (` `) is stripped after `field:`;
//! - multiple `data:` lines in one event are joined with `\n`;
//! - the `event:` field names the event's type (defaulting to `"message"` when absent or empty);
//! - a blank line completes and emits the event.
//!
//! Each caller chooses its own policy explicitly at construction and end-of-stream:
//! - UTF-8 decoding ([`Utf8::Strict`] or [`Utf8::Lenient`]);
//! - an optional byte limit over the stream (`max_bytes`);
//! - whether an unterminated trailing event at EOF is rejected ([`Reader::has_pending`])
//!   or flushed ([`Reader::finish_unterminated`]).
//!
//! Complete lines that lie wholly within a pushed slice are decoded in place without
//! copying into the line buffer, and the internal line/event/data buffers are reused
//! across lines and events.

use std::fmt;
use std::ops::ControlFlow;

/// How an event stream reader decodes each line's bytes as UTF-8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Utf8 {
    /// Every completed line must be valid UTF-8; invalid bytes fail with [`Error::InvalidUtf8`].
    Strict,
    /// Invalid UTF-8 sequences are replaced with `U+FFFD` (`\u{FFFD}`), per line.
    Lenient,
}

/// Why feeding bytes into an event stream failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// More bytes arrived than the stream's configured limit allows.
    TooLarge,
    /// A line of a strict stream was not valid UTF-8.
    InvalidUtf8,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("event stream exceeds byte limit"),
            Self::InvalidUtf8 => f.write_str("event stream is not valid UTF-8"),
        }
    }
}

impl std::error::Error for Error {}

/// One completed server-sent event, borrowed from the reader's buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event<'a> {
    /// The event's `event:` field when non-empty, else `None`.
    pub event: Option<&'a str>,
    /// The event's `data:` lines joined with `\n`.
    pub data: &'a str,
}

impl<'a> Event<'a> {
    /// The event's type: its `event:` field when non-empty, else `"message"`.
    #[must_use]
    pub const fn kind(self) -> &'a str {
        match self.event {
            Some(event) if !event.is_empty() => event,
            _ => "message",
        }
    }
}

/// Reusable accumulator for the fields of the event currently being read.
#[derive(Debug, Default)]
struct EventBuf {
    event: String,
    data: String,
    /// Whether any non-comment field line has been seen for this event.
    pending: bool,
    /// Whether at least one `data:` line has been seen for this event.
    has_data: bool,
}

impl EventBuf {
    fn line_ended<B>(
        &mut self,
        line: &str,
        on_event: &mut impl FnMut(Event<'_>) -> ControlFlow<B>,
    ) -> ControlFlow<B> {
        if line.is_empty() {
            if self.pending {
                self.pending = false;
                return self.emit(on_event);
            }
            return ControlFlow::Continue(());
        }
        if line.starts_with(':') {
            return ControlFlow::Continue(());
        }
        self.pending = true;
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.has_data = true;
                self.data.push_str(value);
            }
            "event" => {
                self.event.clear();
                self.event.push_str(value);
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }

    fn emit<B>(
        &mut self,
        on_event: &mut impl FnMut(Event<'_>) -> ControlFlow<B>,
    ) -> ControlFlow<B> {
        let event = Event {
            event: (!self.event.is_empty()).then_some(self.event.as_str()),
            data: &self.data,
        };
        let flow = on_event(event);
        self.event.clear();
        self.data.clear();
        self.has_data = false;
        flow
    }
}

/// Incremental parser for a `text/event-stream` byte stream.
#[derive(Debug)]
pub struct Reader {
    utf8: Utf8,
    max_bytes: Option<usize>,
    read: usize,
    /// Bytes of an unfinished line (and any multi-byte codepoint) cut between pieces.
    line: Vec<u8>,
    buf: EventBuf,
    /// A carriage return just ended a line; an immediately following newline finishes that CRLF.
    cr: bool,
}

impl Reader {
    /// Create a reader with an explicit UTF-8 policy and optional total-stream byte limit.
    #[must_use]
    pub const fn new(utf8: Utf8, max_bytes: Option<usize>) -> Self {
        Self {
            utf8,
            max_bytes,
            read: 0,
            line: Vec::new(),
            buf: EventBuf {
                event: String::new(),
                data: String::new(),
                pending: false,
                has_data: false,
            },
            cr: false,
        }
    }

    /// Whether bytes of an unfinished line or fields of an unterminated event remain.
    #[must_use]
    pub const fn has_pending(&self) -> bool {
        self.buf.pending || !self.line.is_empty()
    }

    /// Feed `bytes` into the stream, calling `on_event` for each event completed by
    /// a blank line.
    ///
    /// Returning [`ControlFlow::Break`] stops reading `bytes` immediately and returns
    /// that value; any remaining bytes in `bytes` are not consumed.
    ///
    /// # Errors
    /// [`Error::TooLarge`] if the total bytes pushed would exceed `max_bytes`
    /// (checked before reading `bytes`), or [`Error::InvalidUtf8`] on a
    /// [`Utf8::Strict`] stream when a completed line is not valid UTF-8.
    pub fn push<B>(
        &mut self,
        mut bytes: &[u8],
        mut on_event: impl FnMut(Event<'_>) -> ControlFlow<B>,
    ) -> Result<ControlFlow<B>, Error> {
        self.read = self.read.saturating_add(bytes.len());
        if self.max_bytes.is_some_and(|max| self.read > max) {
            return Err(Error::TooLarge);
        }
        while !bytes.is_empty() {
            if self.cr {
                self.cr = false;
                if let Some(rest) = bytes.strip_prefix(b"\n") {
                    bytes = rest;
                    continue;
                }
            }
            let Some(end) = bytes.iter().position(|&byte| matches!(byte, b'\r' | b'\n')) else {
                self.line.extend_from_slice(bytes);
                break;
            };
            let Some((head, tail)) = bytes.split_at_checked(end) else {
                break;
            };
            let Some((&delim, rest)) = tail.split_first() else {
                break;
            };
            self.cr = delim == b'\r';
            bytes = rest;
            let flow = if self.line.is_empty() {
                Self::process_line(head, self.utf8, &mut self.buf, &mut on_event)?
            } else {
                self.line.extend_from_slice(head);
                let outcome =
                    Self::process_line(&self.line, self.utf8, &mut self.buf, &mut on_event);
                self.line.clear();
                outcome?
            };
            if let ControlFlow::Break(value) = flow {
                return Ok(ControlFlow::Break(value));
            }
        }
        Ok(ControlFlow::Continue(()))
    }

    /// End the stream and emit a trailing event that had no closing blank line,
    /// including any unfinished final line.
    ///
    /// Callers that require every event to end with a blank line should check
    /// [`Self::has_pending`] instead of calling this method.
    ///
    /// # Errors
    /// [`Error::InvalidUtf8`] on a [`Utf8::Strict`] stream when an unfinished final
    /// line is not valid UTF-8.
    pub fn finish_unterminated<B>(
        &mut self,
        mut on_event: impl FnMut(Event<'_>) -> ControlFlow<B>,
    ) -> Result<ControlFlow<B>, Error> {
        self.cr = false;
        if !self.line.is_empty() {
            let outcome = Self::process_line(&self.line, self.utf8, &mut self.buf, &mut on_event);
            self.line.clear();
            if let ControlFlow::Break(value) = outcome? {
                return Ok(ControlFlow::Break(value));
            }
        }
        if self.buf.pending {
            self.buf.pending = false;
            return Ok(self.buf.emit(&mut on_event));
        }
        Ok(ControlFlow::Continue(()))
    }

    fn process_line<B>(
        raw: &[u8],
        utf8: Utf8,
        buf: &mut EventBuf,
        on_event: &mut impl FnMut(Event<'_>) -> ControlFlow<B>,
    ) -> Result<ControlFlow<B>, Error> {
        match std::str::from_utf8(raw) {
            Ok(line) => Ok(buf.line_ended(line, on_event)),
            Err(_) => match utf8 {
                Utf8::Strict => Err(Error::InvalidUtf8),
                Utf8::Lenient => {
                    let lossy = String::from_utf8_lossy(raw);
                    Ok(buf.line_ended(&lossy, on_event))
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(
        reader: &mut Reader,
        chunks: &[&[u8]],
        unterminated: bool,
    ) -> Result<Vec<(String, String)>, Error> {
        let mut out = Vec::new();
        for chunk in chunks {
            let _ = reader.push(chunk, |event| {
                out.push((event.kind().to_owned(), event.data.to_owned()));
                ControlFlow::<()>::Continue(())
            })?;
        }
        if unterminated {
            let _ = reader.finish_unterminated(|event| {
                out.push((event.kind().to_owned(), event.data.to_owned()));
                ControlFlow::<()>::Continue(())
            })?;
        }
        Ok(out)
    }

    #[test]
    fn line_endings_and_cuts_at_every_byte_boundary_give_identical_events() {
        let streams = [
            "event: text\ndata: hello 🦀\n\n",
            "event: text\r\ndata: hello 🦀\r\n\r\n",
            "event: text\rdata: hello 🦀\r\r",
            "event: text\r\ndata: hello 🦀\n\r\n",
        ];
        for stream in streams {
            for cut in 1..=stream.len() {
                let mut reader = Reader::new(Utf8::Strict, None);
                let chunks: Vec<&[u8]> = stream.as_bytes().chunks(cut).collect();
                let got = collect(&mut reader, &chunks, false).unwrap();
                assert_eq!(
                    got,
                    [("text".to_owned(), "hello 🦀".to_owned())],
                    "cut={cut} stream={stream:?}"
                );
                assert!(!reader.has_pending(), "cut={cut} stream={stream:?}");
            }
        }
    }

    #[test]
    fn empty_chunk_between_cr_and_lf_stays_one_line_end() {
        let mut reader = Reader::new(Utf8::Strict, None);
        let got = collect(
            &mut reader,
            &[
                b"event: text\r",
                b"",
                b"\ndata: one\r",
                b"",
                b"\n\r",
                b"",
                b"\n",
            ],
            false,
        )
        .unwrap();
        assert_eq!(got, [("text".to_owned(), "one".to_owned())]);
        assert!(!reader.has_pending());
    }

    #[test]
    fn data_lines_join_with_newline_and_strip_at_most_one_space() {
        let mut reader = Reader::new(Utf8::Strict, None);
        let stream = concat!(
            "data: first\n",
            "data:  second\n",
            "data:\tthird\n",
            "data:fourth\n",
            "\n",
            "data:\n",
            "data: after-empty\n",
            "\n",
        );
        let got = collect(&mut reader, &[stream.as_bytes()], false).unwrap();
        assert_eq!(
            got,
            [
                (
                    "message".to_owned(),
                    "first\n second\n\tthird\nfourth".to_owned()
                ),
                ("message".to_owned(), "\nafter-empty".to_owned()),
            ]
        );
    }

    #[test]
    fn comments_and_event_type_resets_follow_sse_rules() {
        let mut reader = Reader::new(Utf8::Strict, None);
        let stream = concat!(
            ": keep-alive\n\n",
            "event: custom\n\n",
            "data: plain\n\n",
            "event: first\nevent: second\ndata: x\n\n",
            "event: overridden\nevent:\ndata: y\n\n",
        );
        let got = collect(&mut reader, &[stream.as_bytes()], false).unwrap();
        assert_eq!(
            got,
            [
                ("custom".to_owned(), String::new()),
                ("message".to_owned(), "plain".to_owned()),
                ("second".to_owned(), "x".to_owned()),
                ("message".to_owned(), "y".to_owned()),
            ]
        );
    }

    #[test]
    fn utf8_policy_is_strict_or_lenient_as_chosen() {
        let bad = b"data: hi \xff there\n\n";
        let mut strict = Reader::new(Utf8::Strict, None);
        assert_eq!(collect(&mut strict, &[bad], false), Err(Error::InvalidUtf8));

        let mut lenient = Reader::new(Utf8::Lenient, None);
        assert_eq!(
            collect(&mut lenient, &[bad], false).unwrap(),
            [("message".to_owned(), "hi \u{FFFD} there".to_owned())]
        );
    }

    #[test]
    fn byte_limit_enforces_exact_cap_before_reading_overflowing_chunk() {
        let stream = b"data: ok\n\n";
        let mut exact = Reader::new(Utf8::Strict, Some(stream.len()));
        assert_eq!(
            collect(&mut exact, &[stream], false).unwrap(),
            [("message".to_owned(), "ok".to_owned())]
        );

        let mut capped = Reader::new(Utf8::Strict, Some(stream.len() - 1));
        let mut seen = 0_usize;
        let result = capped.push(stream, |_| {
            seen += 1;
            ControlFlow::<()>::Continue(())
        });
        assert_eq!(result, Err(Error::TooLarge));
        assert_eq!(seen, 0);
    }

    #[test]
    fn unterminated_trailing_event_is_pending_until_flushed() {
        for raw in [
            &b"data: tail"[..],
            &b"data: tail\n"[..],
            &b"data: tail\r\n"[..],
            &b"data: tail\r"[..],
        ] {
            let mut reader = Reader::new(Utf8::Lenient, None);
            assert!(collect(&mut reader, &[raw], false).unwrap().is_empty());
            assert!(reader.has_pending());
            let flushed = collect(&mut reader, &[], true).unwrap();
            assert_eq!(flushed, [("message".to_owned(), "tail".to_owned())]);
            assert!(!reader.has_pending());
        }
    }

    #[test]
    fn control_flow_break_stops_reading_immediately() {
        let mut reader = Reader::new(Utf8::Strict, None);
        let mut seen = Vec::new();
        let flow = reader
            .push(b"data: one\n\ndata: two\n\n", |event| {
                seen.push(event.data.to_owned());
                ControlFlow::Break("stopped")
            })
            .unwrap();
        assert_eq!(flow, ControlFlow::Break("stopped"));
        assert_eq!(seen, ["one"]);
    }
}
