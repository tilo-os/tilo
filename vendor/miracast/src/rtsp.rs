//! RTSP messages as Wi-Fi Display uses them: requests and responses with
//! `text/parameters` bodies, over one TCP connection that both ends send
//! requests on.

use std::fmt::Write as _;
use std::io::{self, Read, Write};

/// The longest header block accepted. WFD messages are a few hundred bytes.
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// The longest body accepted.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// The first line of a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartLine {
    /// `METHOD uri RTSP/1.0`
    Request {
        /// `OPTIONS`, `GET_PARAMETER`, ...
        method: String,
        /// The request URI.
        uri: String,
    },
    /// `RTSP/1.0 code reason`
    Response {
        /// The status code.
        code: u16,
        /// The reason phrase.
        reason: String,
    },
}

/// One RTSP message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Request or response line.
    pub start: StartLine,
    /// Headers in the order they arrived or will be sent.
    pub headers: Vec<(String, String)>,
    /// The body; WFD bodies are text.
    pub body: String,
}

impl Message {
    /// A request without headers.
    #[must_use]
    pub fn request(method: &str, uri: &str, cseq: u32) -> Self {
        Self {
            start: StartLine::Request {
                method: method.to_owned(),
                uri: uri.to_owned(),
            },
            headers: vec![("CSeq".to_owned(), cseq.to_string())],
            body: String::new(),
        }
    }

    /// A response to `request` with `code`.
    #[must_use]
    pub fn response_to(request: &Self, code: u16) -> Self {
        Self {
            start: StartLine::Response {
                code,
                reason: reason_phrase(code).to_owned(),
            },
            headers: vec![(
                "CSeq".to_owned(),
                request.header("CSeq").unwrap_or("0").to_owned(),
            )],
            body: String::new(),
        }
    }

    /// Adds a header.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    /// Sets a `text/parameters` body.
    #[must_use]
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    /// The first header called `name`, compared without case.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The `CSeq` header.
    #[must_use]
    pub fn cseq(&self) -> Option<u32> {
        self.header("CSeq")?.trim().parse().ok()
    }

    /// The method, for a request.
    #[must_use]
    pub fn method(&self) -> Option<&str> {
        match &self.start {
            StartLine::Request { method, .. } => Some(method),
            StartLine::Response { .. } => None,
        }
    }

    /// The status code, for a response.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match &self.start {
            StartLine::Response { code, .. } => Some(*code),
            StartLine::Request { .. } => None,
        }
    }

    /// The `Session` header's identifier, without its `;timeout=` part.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        let value = self.header("Session")?;
        Some(value.split(';').next().unwrap_or(value).trim())
    }

    /// The message as sent on the wire.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut text = String::new();
        match &self.start {
            StartLine::Request { method, uri } => {
                let _ = write!(text, "{method} {uri} RTSP/1.0\r\n");
            }
            StartLine::Response { code, reason } => {
                let _ = write!(text, "RTSP/1.0 {code} {reason}\r\n");
            }
        }
        for (name, value) in &self.headers {
            let _ = write!(text, "{name}: {value}\r\n");
        }
        if !self.body.is_empty() {
            text.push_str("Content-Type: text/parameters\r\n");
        }
        let _ = write!(text, "Content-Length: {}\r\n\r\n", self.body.len());
        text.push_str(&self.body);
        text.into_bytes()
    }

    /// Writes the message to `out`.
    ///
    /// # Errors
    ///
    /// The write failed.
    pub fn write_to(&self, out: &mut impl Write) -> io::Result<()> {
        out.write_all(&self.encode())?;
        out.flush()
    }

    fn parse(head: &str, body: String) -> io::Result<Self> {
        let mut lines = head.split("\r\n").flat_map(|line| line.split('\n'));
        let first = lines.next().unwrap_or_default().trim();
        let start = parse_start_line(first)
            .ok_or_else(|| invalid(format!("not an RTSP start line: {first:?}")))?;
        let headers = lines
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                Some((name.trim().to_owned(), value.trim().to_owned()))
            })
            .collect();
        Ok(Self {
            start,
            headers,
            body,
        })
    }
}

fn parse_start_line(line: &str) -> Option<StartLine> {
    if let Some(rest) = line.strip_prefix("RTSP/") {
        let mut parts = rest.splitn(3, ' ');
        let _version = parts.next()?;
        let code = parts.next()?.parse().ok()?;
        let reason = parts.next().unwrap_or_default().to_owned();
        return Some(StartLine::Response { code, reason });
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let uri = parts.next()?.to_owned();
    parts
        .next()
        .filter(|version| version.starts_with("RTSP/"))?;
    Some(StartLine::Request { method, uri })
}

/// The reason phrase RFC 2326 gives `code`.
#[must_use]
pub fn reason_phrase(code: u16) -> &'static str {
    match code {
        200 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        451 => "Parameter Not Understood",
        454 => "Session Not Found",
        455 => "Method Not Valid In This State",
        457 => "Invalid Range",
        461 => "Unsupported Transport",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Reads messages off a stream. A read that times out (a socket with a read
/// timeout) returns its error without losing the bytes read so far, so the
/// caller can do other work and call again.
#[derive(Debug)]
pub struct MessageReader<R> {
    inner: R,
    buffer: Vec<u8>,
}

impl<R: Read> MessageReader<R> {
    /// Reads from `inner`.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
        }
    }

    /// The stream being read.
    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// The next message, or `None` at a clean end of the stream.
    ///
    /// # Errors
    ///
    /// The read failed or timed out, or the peer sent something that is not
    /// RTSP.
    pub fn read_message(&mut self) -> io::Result<Option<Message>> {
        loop {
            if let Some(message) = self.take_message()? {
                return Ok(Some(message));
            }
            let mut chunk = [0_u8; 4096];
            let read = self.inner.read(&mut chunk)?;
            if read == 0 {
                return if self.buffer.iter().all(u8::is_ascii_whitespace) {
                    Ok(None)
                } else {
                    Err(io::ErrorKind::UnexpectedEof.into())
                };
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    fn take_message(&mut self) -> io::Result<Option<Message>> {
        // Blank lines between messages are keep-alives some sinks send.
        let leading = self
            .buffer
            .iter()
            .take_while(|byte| matches!(byte, b'\r' | b'\n'))
            .count();
        self.buffer.drain(..leading);
        let Some(head_end) = find_head_end(&self.buffer) else {
            if self.buffer.len() > MAX_HEADER_BYTES {
                return Err(invalid("RTSP header block too long".to_owned()));
            }
            return Ok(None);
        };
        let head = std::str::from_utf8(&self.buffer[..head_end.start])
            .map_err(|_| invalid("RTSP headers are not UTF-8".to_owned()))?
            .to_owned();
        let message = Message::parse(&head, String::new())?;
        let length = match message.header("Content-Length") {
            Some(value) => value
                .trim()
                .parse::<usize>()
                .map_err(|_| invalid(format!("bad Content-Length {value:?}")))?,
            None => 0,
        };
        if length > MAX_BODY_BYTES {
            return Err(invalid(format!("RTSP body of {length} bytes is too long")));
        }
        let total = head_end.end + length;
        if self.buffer.len() < total {
            return Ok(None);
        }
        let body = String::from_utf8_lossy(&self.buffer[head_end.end..total]).into_owned();
        self.buffer.drain(..total);
        Ok(Some(Message { body, ..message }))
    }
}

/// Where the header block ends: the blank line, as `start..end` of the
/// terminator. Bare `\n` line endings are accepted.
fn find_head_end(buffer: &[u8]) -> Option<std::ops::Range<usize>> {
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at..at + 4);
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|at| at..at + 2);
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(if a.start <= b.start { a } else { b }),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_round_trips_with_its_body() {
        let request = Message::request("SET_PARAMETER", "rtsp://localhost/wfd1.0", 4)
            .with_body("wfd_trigger_method: SETUP\r\n");
        let wire = request.encode();
        let text = String::from_utf8(wire.clone()).expect("text");
        assert!(text.starts_with("SET_PARAMETER rtsp://localhost/wfd1.0 RTSP/1.0\r\nCSeq: 4\r\n"));
        assert!(text.contains("Content-Type: text/parameters\r\nContent-Length: 27\r\n\r\n"));
        let mut reader = MessageReader::new(wire.as_slice());
        let read = reader.read_message().expect("read").expect("message");
        assert_eq!(read.method(), Some("SET_PARAMETER"));
        assert_eq!(read.cseq(), Some(4));
        assert_eq!(read.body, "wfd_trigger_method: SETUP\r\n");
        assert!(reader.read_message().expect("eof").is_none());
    }

    #[test]
    fn messages_split_across_reads_and_packed_together_both_parse() {
        struct Trickle<'a>(&'a [u8]);
        impl Read for Trickle<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let Some((first, rest)) = self.0.split_first() else {
                    return Ok(0);
                };
                buf[0] = *first;
                self.0 = rest;
                Ok(1)
            }
        }
        let mut wire = Message::request("OPTIONS", "*", 1)
            .with_header("Require", "org.wfa.wfd1.0")
            .encode();
        let response = Message::response_to(&Message::request("OPTIONS", "*", 1), 200)
            .with_header("Session", "1234;timeout=30");
        wire.extend(response.encode());

        let mut reader = MessageReader::new(Trickle(&wire));
        let first = reader.read_message().expect("read").expect("first");
        assert_eq!(first.header("require"), Some("org.wfa.wfd1.0"));
        let second = reader.read_message().expect("read").expect("second");
        assert_eq!(second.status(), Some(200));
        assert_eq!(second.session_id(), Some("1234"));
        assert!(reader.read_message().expect("eof").is_none());
    }

    #[test]
    fn garbage_and_oversized_bodies_are_errors() {
        let mut reader = MessageReader::new(&b"HELLO\r\n\r\n"[..]);
        assert!(reader.read_message().is_err());
        let mut reader =
            MessageReader::new(&b"RTSP/1.0 200 OK\r\nContent-Length: 999999\r\n\r\n"[..]);
        assert!(reader.read_message().is_err());
        let mut reader = MessageReader::new(&b"RTSP/1.0 200 OK\r\nContent-Length: 4\r\n\r\nab"[..]);
        assert_eq!(
            reader.read_message().expect_err("truncated").kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
