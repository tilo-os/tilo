//! The little of HTTP/1.1 an endpoint of a served run speaks: one request a
//! connection, its body sized or chunked, and an answer whose body is written
//! as it comes and ends the connection.

use std::io::{self, BufRead, Read, Write};

use legacy_agent_protocol::served::{MAX_BODY_CHUNK, stays_behind};

/// The most a request's line and headers may take.
pub const MAX_HEAD_BYTES: usize = 64 * 1024;

const MAX_HEADERS: usize = 128;

/// A request's line and headers.
#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    /// The target as written: a path, with its query.
    pub target: String,
    /// Names in lower case, values trimmed, in the order sent.
    pub headers: Vec<(String, String)>,
}

/// How long a request's body is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyLength {
    None,
    Sized(u64),
    Chunked,
}

/// Why a request could not be read.
#[derive(Debug)]
pub enum Malformed {
    /// The asker left before its request was whole.
    Closed,
    Io(io::Error),
    /// What is wrong with it, in words for its answer.
    Bad(&'static str),
}

impl From<io::Error> for Malformed {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl Head {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn body(&self) -> Result<BodyLength, Malformed> {
        if let Some(encoding) = self.header("transfer-encoding") {
            return if encoding.to_ascii_lowercase().contains("chunked") {
                Ok(BodyLength::Chunked)
            } else {
                Err(Malformed::Bad("a transfer encoding other than chunked"))
            };
        }
        match self.header("content-length") {
            None => Ok(BodyLength::None),
            Some(length) => match length.parse::<u64>() {
                Ok(0) => Ok(BodyLength::None),
                Ok(length) => Ok(BodyLength::Sized(length)),
                Err(_) => Err(Malformed::Bad("a content length that is no number")),
            },
        }
    }

    /// Whether the asker waits for a go-ahead before it sends its body.
    #[must_use]
    pub fn expects_continue(&self) -> bool {
        self.header("expect")
            .is_some_and(|expect| expect.eq_ignore_ascii_case("100-continue"))
    }

    /// The headers that leave the machine with the request.
    #[must_use]
    pub fn carried(&self) -> Vec<(String, String)> {
        self.headers
            .iter()
            .filter(|(name, _)| !stays_behind(name))
            .cloned()
            .collect()
    }
}

/// One line of a request's head, without its line ending, counted against
/// `left`.
fn line<R: BufRead>(reader: &mut R, left: &mut usize) -> Result<String, Malformed> {
    let mut bytes = Vec::new();
    let most = u64::try_from(left.saturating_add(1)).unwrap_or(u64::MAX);
    let read = reader.by_ref().take(most).read_until(b'\n', &mut bytes)?;
    if read == 0 {
        return Err(Malformed::Closed);
    }
    if bytes.last() != Some(&b'\n') {
        return Err(if read > *left {
            Malformed::Bad("a request head that is too long")
        } else {
            Malformed::Closed
        });
    }
    *left = left.saturating_sub(read);
    bytes.pop();
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Read one request's line and headers.
pub fn read_head<R: BufRead>(reader: &mut R) -> Result<Head, Malformed> {
    let mut left = MAX_HEAD_BYTES;
    let first = line(reader, &mut left)?;
    let mut words = first.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return Err(Malformed::Bad("a request line that is not one"));
    };
    if method.is_empty() || !method.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(Malformed::Bad("a method that is not one"));
    }
    if !target.starts_with('/') || !version.starts_with("HTTP/1.") {
        return Err(Malformed::Bad("a request line that is not one"));
    }
    let mut headers = Vec::new();
    loop {
        let header = line(reader, &mut left)?;
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(Malformed::Bad("a header without a name"));
        };
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(Malformed::Bad("a header without a name"));
        }
        if headers.len() >= MAX_HEADERS {
            return Err(Malformed::Bad("too many headers"));
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    Ok(Head {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
    })
}

/// Read `count` bytes, handing them to `each` in pieces of at most
/// [`MAX_BODY_CHUNK`]; the last piece is marked when `ends`.
fn pieces<R: BufRead>(
    reader: &mut R,
    count: u64,
    ends: bool,
    each: &mut dyn FnMut(&[u8], bool) -> io::Result<()>,
) -> Result<(), Malformed> {
    let mut buffer = vec![0_u8; MAX_BODY_CHUNK];
    let mut left = count;
    while left > 0 {
        let want = usize::try_from(left).map_or(MAX_BODY_CHUNK, |left| left.min(MAX_BODY_CHUNK));
        let Some(space) = buffer.get_mut(..want) else {
            return Err(Malformed::Bad("a body that cannot be read"));
        };
        let read = reader.read(space)?;
        if read == 0 {
            return Err(Malformed::Closed);
        }
        left = left.saturating_sub(u64::try_from(read).unwrap_or(u64::MAX));
        each(buffer.get(..read).unwrap_or_default(), ends && left == 0)?;
    }
    Ok(())
}

/// Read a request's body of `length`, handing it to `each` piece by piece;
/// the last call says so, and there is always one.
pub fn read_body<R: BufRead>(
    reader: &mut R,
    length: BodyLength,
    each: &mut dyn FnMut(&[u8], bool) -> io::Result<()>,
) -> Result<(), Malformed> {
    match length {
        BodyLength::None => Ok(each(&[], true)?),
        BodyLength::Sized(count) => pieces(reader, count, true, each),
        BodyLength::Chunked => loop {
            let mut left = MAX_HEAD_BYTES;
            let size = line(reader, &mut left)?;
            let digits = size.split(';').next().unwrap_or_default().trim();
            let Ok(count) = u64::from_str_radix(digits, 16) else {
                return Err(Malformed::Bad("a chunk without a size"));
            };
            if count == 0 {
                // Trailers, if any, then the empty line that ends them.
                while !line(reader, &mut left)?.is_empty() {}
                return Ok(each(&[], true)?);
            }
            pieces(reader, count, false, each)?;
            if !line(reader, &mut left)?.is_empty() {
                return Err(Malformed::Bad("a chunk longer than its size"));
            }
        },
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Content Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}

/// An answer on its way to the asker: its head is written, its body follows
/// in chunks, and its end closes the connection.
#[derive(Debug)]
pub struct Answer {
    /// A status that carries no body is written without one.
    bodied: bool,
}

impl Answer {
    /// Write the head of an answer with `status` and the host's `headers`,
    /// leaving out the ones that frame a connection: this one is framed
    /// here.
    pub fn begin<W: Write>(
        writer: &mut W,
        status: u16,
        headers: &[(String, String)],
    ) -> io::Result<Self> {
        let bodied = status >= 200 && status != 204 && status != 304;
        let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
        for (name, value) in headers {
            let plain = |text: &str| !text.contains(['\r', '\n']);
            if stays_behind(&name.to_ascii_lowercase()) || !plain(name) || !plain(value) {
                continue;
            }
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        if bodied {
            head.push_str("transfer-encoding: chunked\r\n");
        }
        head.push_str("connection: close\r\n\r\n");
        writer.write_all(head.as_bytes())?;
        writer.flush()?;
        Ok(Self { bodied })
    }

    /// Write more of the body.
    pub fn body<W: Write>(&self, writer: &mut W, bytes: &[u8]) -> io::Result<()> {
        if !self.bodied || bytes.is_empty() {
            return Ok(());
        }
        write!(writer, "{:x}\r\n", bytes.len())?;
        writer.write_all(bytes)?;
        writer.write_all(b"\r\n")?;
        writer.flush()
    }

    /// End the body.
    pub fn end<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        if self.bodied {
            writer.write_all(b"0\r\n\r\n")?;
        }
        writer.flush()
    }
}

/// Answer a request that is not carried to the host, in words.
pub fn refuse<W: Write>(writer: &mut W, status: u16, words: &str) -> io::Result<()> {
    write!(
        writer,
        "HTTP/1.1 {status} {}\r\ncontent-type: text/plain; charset=utf-8\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{words}",
        reason(status),
        words.len()
    )?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn body_of(request: &[u8]) -> (Head, Vec<(Vec<u8>, bool)>) {
        let mut reader = Cursor::new(request.to_vec());
        let head = read_head(&mut reader).unwrap();
        let mut seen = Vec::new();
        read_body(&mut reader, head.body().unwrap(), &mut |bytes, last| {
            seen.push((bytes.to_vec(), last));
            Ok(())
        })
        .unwrap();
        (head, seen)
    }

    tilo_test_env::scenario! {
        fn an_endpoint_reads_one_request_and_writes_one_answer;

        phase a_sized_request_is_read_whole_and_its_sign_in_stays_behind;
        {
            let (head, body) = body_of(
                b"POST /v1/messages?beta=true HTTP/1.1\r\nHost: 127.0.0.1:4000\r\n\
                  Authorization: Bearer nothing\r\nX-Api-Key: nothing\r\n\
                  Content-Type: application/json\r\nContent-Length: 5\r\n\r\nhello",
            );
            assert_eq!(
                (head.method.as_str(), head.target.as_str()),
                ("POST", "/v1/messages?beta=true")
            );
            assert_eq!(
                head.carried(),
                [("content-type".to_owned(), "application/json".to_owned())]
            );
            assert_eq!(body, [(b"hello".to_vec(), true)]);
        }

        phase a_request_without_a_body_still_ends_once;
        {
            let (head, body) = body_of(b"GET /mcp HTTP/1.1\r\nAccept: text/event-stream\r\n\r\n");
            assert_eq!(head.body().unwrap(), BodyLength::None);
            assert_eq!(body, [(Vec::new(), true)]);
        }

        phase a_chunked_body_is_read_to_its_last_chunk;
        {
            let (head, body) = body_of(
                b"POST /repo.git/git-receive-pack HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\
                  Expect: 100-continue\r\n\r\n4\r\nwxyz\r\n3;note=1\r\nabc\r\n0\r\nTrailer: 1\r\n\r\n",
            );
            assert!(head.expects_continue());
            assert_eq!(
                body,
                [
                    (b"wxyz".to_vec(), false),
                    (b"abc".to_vec(), false),
                    (Vec::new(), true)
                ]
            );
        }

        phase a_long_body_travels_in_pieces_a_frame_can_carry;
        {
            let mut request =
                format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY_CHUNK + 10)
                    .into_bytes();
            request.extend(std::iter::repeat_n(b'x', MAX_BODY_CHUNK + 10));
            let (_, body) = body_of(&request);
            assert_eq!(
                body.iter().map(|(bytes, last)| (bytes.len(), *last)).collect::<Vec<_>>(),
                [(MAX_BODY_CHUNK, false), (10, true)]
            );
        }

        phase what_is_no_request_is_said_so_and_a_cut_one_is_closed;
        {
            for (request, why) in [
                (&b"hello there\r\n\r\n"[..], "a request line that is not one"),
                (&b"get / HTTP/1.1\r\n\r\n"[..], "a method that is not one"),
                (
                    &b"GET / HTTP/1.1\r\nno colon here\r\n\r\n"[..],
                    "a header without a name",
                ),
            ] {
                match read_head(&mut Cursor::new(request.to_vec())) {
                    Err(Malformed::Bad(said)) => assert_eq!(said, why),
                    other => panic!("{other:?}"),
                }
            }
            assert!(matches!(
                read_head(&mut Cursor::new(b"GET / HTTP/1.1\r\nAccept: a".to_vec())),
                Err(Malformed::Closed)
            ));
            let mut long = b"GET / HTTP/1.1\r\nX: ".to_vec();
            long.extend(std::iter::repeat_n(b'a', MAX_HEAD_BYTES));
            assert!(matches!(
                read_head(&mut Cursor::new(long)),
                Err(Malformed::Bad("a request head that is too long"))
            ));
            let mut cut = Cursor::new(b"POST / HTTP/1.1\r\nContent-Length: 9\r\n\r\nabc".to_vec());
            let head = read_head(&mut cut).unwrap();
            assert!(matches!(
                read_body(&mut cut, head.body().unwrap(), &mut |_, _| Ok(())),
                Err(Malformed::Closed)
            ));
        }

        phase an_answer_is_framed_here_whatever_the_host_said_of_framing;
        {
            let mut wire = Vec::new();
            let answer = Answer::begin(
                &mut wire,
                200,
                &[
                    ("content-type".into(), "text/event-stream".into()),
                    ("Content-Length".into(), "999".into()),
                    ("x-bad".into(), "a\r\nset-cookie: b".into()),
                ],
            )
            .unwrap();
            answer.body(&mut wire, b"data: 1\n\n").unwrap();
            answer.body(&mut wire, b"").unwrap();
            answer.end(&mut wire).unwrap();
            assert_eq!(
                String::from_utf8(wire).unwrap(),
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                 transfer-encoding: chunked\r\nconnection: close\r\n\r\n\
                 9\r\ndata: 1\n\n\r\n0\r\n\r\n"
            );
        }

        phase a_status_without_a_body_gets_none;
        {
            let mut wire = Vec::new();
            let answer = Answer::begin(&mut wire, 204, &[]).unwrap();
            answer.body(&mut wire, b"ignored").unwrap();
            answer.end(&mut wire).unwrap();
            assert_eq!(
                String::from_utf8(wire).unwrap(),
                "HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n"
            );
            let mut wire = Vec::new();
            refuse(&mut wire, 400, "a header without a name").unwrap();
            assert!(String::from_utf8(wire).unwrap().ends_with(
                "content-length: 23\r\nconnection: close\r\n\r\na header without a name"
            ));
        }
    }
}
