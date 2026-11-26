//! Provider-neutral HTTP helpers shared by cloud providers: reading responses
//! and, behind the `client` feature, building the shared blocking client.

use std::io::{self, Read};
use std::ops::Range;
use std::time::{Duration, SystemTime};

use crate::{CredentialSource, Download, Error, Result};

/// Build the shared blocking HTTP client for cloud storage providers: Tilo's
/// TLS defaults, the `Tilo-Storage/0.1` user agent, a 30-minute whole-request
/// timeout for streaming uploads, ranged reads and long polls, a 10-second
/// connect timeout, and redirects disabled so providers handle 3xx responses
/// explicitly without forwarding credentials.
///
/// # Errors
/// [`Error::Offline`] when the HTTP client cannot be built.
#[cfg(all(feature = "client", not(target_family = "wasm")))]
pub fn blocking_client() -> Result<reqwest::blocking::Client> {
    tilo_net::http::blocking_client()
        .user_agent("Tilo-Storage/0.1")
        // Whole-request deadline: chunk uploads, large ranged reads and long
        // polls stream for a while; the connect timeout catches dead servers.
        .timeout(Duration::from_secs(30 * 60))
        .connect_timeout(Duration::from_secs(10))
        // Redirects are handled explicitly so credentials are never forwarded
        // and non-redirect 3xx responses (such as 308 Resume Incomplete) reach
        // the provider unchanged.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::Offline)
}

/// `Content-Range: bytes <first>-<last>/<total|*>` as the half-open range
/// `first..last + 1` and the total, `None` when the total is `*`.
///
/// Refuses another unit, `last < first` and a malformed total; accepts any
/// whitespace after `bytes`.
pub fn parse_content_range(value: &str) -> Option<(Range<u64>, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let (span, total) = rest.trim_start().split_once('/')?;
    let (first, last) = span.split_once('-')?;
    let first: u64 = first.trim().parse().ok()?;
    let last: u64 = last.trim().parse().ok()?;
    let total = match total.trim() {
        "*" => None,
        total => Some(total.parse().ok()?),
    };
    (last >= first).then_some((first..last.checked_add(1)?, total))
}

/// `Retry-After` (RFC 9110 §10.2.3): delay-seconds, fractional seconds as
/// some servers send them, or an HTTP-date, read relative to `now`. A date
/// already past means "now" ([`Duration::ZERO`]).
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    if let Ok(seconds) = value.parse::<f64>() {
        return Duration::try_from_secs_f64(seconds).ok();
    }
    let at = parse_http_date(value)?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// The provider-neutral reading of a failed status, under the rate-limit
/// policy documented on [`Error::RateLimited`].
///
/// `None` for 401 and 409: whether a 401 rejects the account's credential
/// (or only a pre-authenticated URL, or an unauthenticated request) and
/// what a 409 conflicts with are the provider's call. Every other status
/// has an answer, [`Error::Status`] when nothing more specific applies.
pub fn classify(status: u16, retry_after: Option<Duration>) -> Option<Error> {
    Some(match status {
        401 | 409 => return None,
        300..=399 => Error::InvalidResponse("unexpected redirect"),
        403 => Error::Forbidden,
        404 | 410 => Error::NotFound,
        412 => Error::Conflict { current: None },
        413 => Error::TooLarge,
        // 509 is the "bandwidth limit exceeded" throttle of cPanel and
        // SharePoint.
        429 | 503 | 509 => Error::RateLimited { retry_after },
        507 => Error::InsufficientStorage,
        500..=599 if retry_after.is_some() => Error::RateLimited { retry_after },
        status => Error::Status(status),
    })
}

/// The server rejected the account's credential: report it, so the next
/// request gets a fresh one (or the account asks for sign-in), and fail with
/// [`Error::Unauthorized`].
pub fn unauthorized(credentials: &dyn CredentialSource) -> Error {
    credentials.rejected();
    Error::Unauthorized
}

/// Read at most `limit` bytes of a body. A body that declares (`declared`,
/// its `Content-Length`) or streams more is refused without buffering past
/// the limit.
///
/// # Errors
/// [`Error::TooLarge`] past the limit, [`Error::Io`] when reading fails.
pub fn read_bounded(body: impl Read, declared: Option<u64>, limit: usize) -> Result<Vec<u8>> {
    let limit64 = u64::try_from(limit).unwrap_or(u64::MAX);
    if declared.is_some_and(|len| len > limit64) {
        return Err(Error::TooLarge);
    }
    let mut bytes = Vec::new();
    body.take(limit64.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::TooLarge);
    }
    Ok(bytes)
}

/// Read a JSON body of at most `limit` bytes, by [`read_bounded`].
///
/// # Errors
/// What [`read_bounded`] returns, and [`Error::InvalidResponse`] for a body
/// that is not one JSON value of type `T`.
pub fn read_json<T: serde::de::DeserializeOwned>(
    body: impl Read,
    declared: Option<u64>,
    limit: usize,
) -> Result<T> {
    let bytes = read_bounded(body, declared, limit)?;
    serde_json::from_slice(&bytes).map_err(|_| Error::InvalidResponse("json"))
}

/// Reads exactly `len` bytes and no more; an early end of stream is
/// [`io::ErrorKind::UnexpectedEof`], so a cut-off body never passes for a
/// short file.
pub struct Exact<R> {
    inner: R,
    remaining: u64,
}

impl<R> Exact<R> {
    pub fn new(inner: R, len: u64) -> Self {
        Self {
            inner,
            remaining: len,
        }
    }
}

impl<R: Read> Read for Exact<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let read = self.inner.read(buf.get_mut(..want).unwrap_or_default())?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the download ended before its length",
            ));
        }
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Served {
    pub partial: bool,
    pub content_range: Option<(Range<u64>, Option<u64>)>,
    pub declared: Option<u64>,
}

impl Served {
    /// The item's total size as the response states it: the total of a
    /// 206's `Content-Range`, or the length of a whole body.
    pub fn total(&self) -> Option<u64> {
        if self.partial {
            self.content_range.as_ref().and_then(|(_, total)| *total)
        } else {
            self.declared
        }
    }
}

/// Turn a download response's `body` into exactly the bytes of `wanted`
/// (the whole item when `None`).
///
/// A 206 must start at `wanted.start` and cover it; a 200 that ignored the
/// `Range` header has its prefix skipped. `wanted` is clamped to the total
/// the response states. The body is [`Exact`]: a response cut short is an
/// error while reading, never a short file. A whole body of undeclared
/// length streams as it comes, with `range` ending at `u64::MAX`.
///
/// The returned [`Download`] carries no revision; the provider sets it.
///
/// # Errors
/// [`Error::InvalidResponse`] for an unsolicited 206, a 206 that does not
/// cover `wanted`, or a 200 shorter than the skipped prefix; [`Error::Io`]
/// while skipping.
pub fn ranged_body(
    body: Box<dyn Read + Send>,
    wanted: Option<Range<u64>>,
    served: Served,
) -> Result<Download> {
    let total = served.total();
    let Some(wanted) = wanted else {
        // A partial response cannot satisfy a whole-file request, even if
        // its Content-Length accurately describes the truncated body.
        if served.partial {
            return Err(Error::InvalidResponse("unexpected partial body"));
        }
        let body: Box<dyn Read + Send> = match served.declared {
            Some(len) => Box::new(Exact::new(body, len)),
            None => body,
        };
        return Ok(Download {
            body,
            range: 0..served.declared.unwrap_or(u64::MAX),
            total,
            revision: None,
        });
    };
    let end = total.map_or(wanted.end, |total| wanted.end.min(total));
    let wanted = wanted.start..end.max(wanted.start);
    let mut body = body;
    if served.partial {
        let (got, _) = served
            .content_range
            .ok_or(Error::InvalidResponse("content-range"))?;
        if got.start != wanted.start || got.end < wanted.end {
            return Err(Error::InvalidResponse("content-range"));
        }
    } else {
        let skipped = io::copy(&mut (&mut body).take(wanted.start), &mut io::sink())?;
        if skipped != wanted.start {
            return Err(Error::InvalidResponse("short body"));
        }
    }
    let len = wanted.end.saturating_sub(wanted.start);
    Ok(Download {
        body: Box::new(Exact::new(body, len)),
        range: wanted,
        total,
        revision: None,
    })
}

/// An RFC 3339 timestamp (`2024-05-01T12:30:45.123Z`, or with a numeric
/// offset), to the nanosecond. `None` for anything else.
pub fn parse_rfc3339(text: &str) -> Option<SystemTime> {
    text.trim()
        .parse::<jiff::Timestamp>()
        .ok()
        .map(SystemTime::from)
}

/// `time` in RFC 3339 UTC, to the second: `2024-05-01T12:30:45Z`.
#[must_use]
pub fn format_rfc3339(time: SystemTime) -> String {
    jiff::Timestamp::try_from(time).map_or_else(|_| String::new(), |at| format!("{at:.0}"))
}

/// `time` in RFC 3339 UTC, to the millisecond: `2024-05-01T12:30:45.123Z`.
#[must_use]
pub fn format_rfc3339_millis(time: SystemTime) -> String {
    jiff::Timestamp::try_from(time).map_or_else(|_| String::new(), |at| format!("{at:.3}"))
}

/// `time` as an HTTP date in GMT: `Sun, 06 Nov 1994 08:49:37 GMT`.
#[must_use]
pub fn format_http_date(time: SystemTime) -> String {
    jiff::Timestamp::try_from(time)
        .ok()
        .and_then(|at| {
            jiff::fmt::rfc2822::DateTimePrinter::new()
                .timestamp_to_rfc9110_string(&at)
                .ok()
        })
        .unwrap_or_default()
}

pub fn parse_http_date(text: &str) -> Option<SystemTime> {
    jiff::fmt::rfc2822::parse(text.trim())
        .ok()
        .map(|at| SystemTime::from(at.timestamp()))
}
