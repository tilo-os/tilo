//! The provider-neutral reading of HTTP responses, shared by every provider.
//!
//! Each provider speaks its own API over its own client, but what a status
//! means, how `Content-Range` and `Retry-After` read, how much of a body is
//! buffered and how a ranged read is cut from a response are one policy.
//! These helpers are pure: they take statuses, header values and readers,
//! so this crate stays free of an HTTP client. Provider-specific rules (a
//! route error, a quota reason, a pre-authenticated URL) are matched by the
//! provider first; [`classify`] is the fallback.

use std::io::{self, Read};
use std::ops::Range;
use std::time::{Duration, SystemTime};

use crate::{CredentialSource, Download, Error, Result};

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

/// What a download response said about its body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Served {
    /// The status was 206 Partial Content.
    pub partial: bool,
    /// Its `Content-Range`, parsed with [`parse_content_range`].
    pub content_range: Option<(Range<u64>, Option<u64>)>,
    /// Its `Content-Length`.
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
/// [`Error::InvalidResponse`] for a 206 that does not cover `wanted` and a
/// 200 shorter than the skipped prefix, [`Error::Io`] while skipping.
pub fn ranged_body(
    body: Box<dyn Read + Send>,
    wanted: Option<Range<u64>>,
    served: Served,
) -> Result<Download> {
    let total = served.total();
    let Some(wanted) = wanted else {
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
        // The server ignored the range and sent the whole body.
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
    chrono::DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(SystemTime::from)
}

/// An HTTP-date (`Sun, 06 Nov 1994 08:49:37 GMT`). `None` for anything else.
pub fn parse_http_date(text: &str) -> Option<SystemTime> {
    chrono::DateTime::parse_from_rfc2822(text.trim())
        .ok()
        .map(SystemTime::from)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn content_range_accepts_valid_and_rejects_inverted_or_malformed() {
        for (value, expected) in [
            ("bytes 0-1023/2048", Some((0..1024, Some(2048)))),
            ("bytes 10-19/100", Some((10..20, Some(100)))),
            ("bytes 2-4/10", Some((2..5, Some(10)))),
            (
                "bytes 1000-60999/100000",
                Some((1000..61_000, Some(100_000))),
            ),
            ("bytes 5-5/*", Some((5..6, None))),
            ("bytes 0-0/*", Some((0..1, None))),
            ("  bytes\t 7-8 / 9 ", Some((7..9, Some(9)))),
            ("bytes 100-50/200", None),
            ("bytes 9-3/10", None),
            ("bytes 5-4/10", None),
            ("bytes 0-9/abc", None),
            ("bytes 0-18446744073709551615/*", None),
            ("bytes0-1/2", None),
            ("items 0-1/2", None),
            ("bytes */100", None),
            ("", None),
        ] {
            assert_eq!(parse_content_range(value), expected, "{value:?}");
        }
    }

    #[test]
    fn retry_after_reads_seconds_fractions_and_http_dates() {
        let now = parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT").unwrap();
        assert_eq!(parse_retry_after(" 7 ", now), Some(Duration::from_secs(7)));
        assert_eq!(parse_retry_after("0", now), Some(Duration::ZERO));
        assert_eq!(
            parse_retry_after("1.5", now),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:30 GMT", now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:00:00 GMT", now),
            Some(Duration::ZERO),
            "a date already past means now"
        );
        assert_eq!(parse_retry_after("-1", now), None);
        assert_eq!(parse_retry_after("NaN", now), None);
        assert_eq!(parse_retry_after("inf", now), None);
        assert_eq!(parse_retry_after("soon", now), None);
        assert_eq!(parse_retry_after("", now), None);
    }

    #[test]
    fn classify_has_one_rate_limit_policy() {
        let hint = Some(Duration::from_secs(3));
        let limited = |status, retry_after| {
            matches!(
                classify(status, retry_after),
                Some(Error::RateLimited { retry_after: got }) if got == retry_after
            )
        };
        for status in [429, 503, 509] {
            assert!(limited(status, hint), "{status} with Retry-After");
            assert!(limited(status, None), "{status} without Retry-After");
        }
        for status in [500, 502, 504] {
            assert!(limited(status, hint), "{status} with Retry-After");
            assert!(
                matches!(classify(status, None), Some(Error::Status(s)) if s == status),
                "{status} without Retry-After"
            );
        }
        for status in [429, 500, 503, 509] {
            assert!(classify(status, hint).unwrap().is_transient());
        }
    }

    #[test]
    fn classify_leaves_credentials_and_conflicts_to_the_provider() {
        assert!(classify(401, None).is_none());
        assert!(classify(409, None).is_none());
        for (status, expected) in [
            (301, "InvalidResponse(\"unexpected redirect\")"),
            (330, "InvalidResponse(\"unexpected redirect\")"),
            (403, "Forbidden"),
            (404, "NotFound"),
            (410, "NotFound"),
            (412, "Conflict { current: None }"),
            (413, "TooLarge"),
            (507, "InsufficientStorage"),
            (422, "Status(422)"),
        ] {
            assert_eq!(
                format!("{:?}", classify(status, None).unwrap()),
                expected,
                "{status}"
            );
        }
        // A Retry-After on a client error is not a rate limit.
        assert!(matches!(
            classify(404, Some(Duration::from_secs(1))),
            Some(Error::NotFound)
        ));
    }

    struct Counting(AtomicUsize);

    impl CredentialSource for Counting {
        fn credential(&self) -> Result<crate::Credential> {
            Err(Error::Unauthorized)
        }

        fn rejected(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn unauthorized_reports_the_rejection() {
        let credentials = Counting(AtomicUsize::new(0));
        assert!(matches!(unauthorized(&credentials), Error::Unauthorized));
        assert_eq!(credentials.0.load(Ordering::SeqCst), 1);
    }

    struct Failing;

    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::ConnectionReset))
        }
    }

    #[test]
    fn bounded_reads_refuse_declared_and_streamed_excess() {
        assert_eq!(read_bounded(&b"abcd"[..], Some(4), 4).unwrap(), b"abcd");
        assert_eq!(read_bounded(&b""[..], None, 4).unwrap(), b"");
        assert!(matches!(
            read_bounded(&b"ab"[..], Some(5), 4),
            Err(Error::TooLarge)
        ));
        assert!(matches!(
            read_bounded(&b"abcde"[..], None, 4),
            Err(Error::TooLarge)
        ));
        assert!(matches!(
            read_bounded(Failing, None, 4),
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::ConnectionReset
        ));
    }

    #[test]
    fn exact_errors_on_early_eof() {
        let mut out = Vec::new();
        let error = Exact::new(&b"abc"[..], 5)
            .read_to_end(&mut out)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(out, b"abc");
        let mut out = Vec::new();
        Exact::new(&b"abcdef"[..], 4).read_to_end(&mut out).unwrap();
        assert_eq!(out, b"abcd");
        let mut out = Vec::new();
        Exact::new(&b""[..], 0).read_to_end(&mut out).unwrap();
        assert!(out.is_empty());
        assert_eq!(Exact::new(&b"abc"[..], 3).read(&mut []).unwrap(), 0);
    }

    fn body(bytes: &[u8]) -> Box<dyn Read + Send> {
        Box::new(io::Cursor::new(bytes.to_vec()))
    }

    fn read(download: Download) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        download.body.take(1 << 20).read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn ranged_body_skips_prefix_of_ignored_range_and_fails_on_short_body() {
        let whole = b"0123456789";
        let ignored = Served {
            partial: false,
            content_range: None,
            declared: Some(10),
        };
        // A 200 that ignored the range: skipped to the start, clamped to
        // the length.
        let download = ranged_body(body(whole), Some(3..20), ignored.clone()).unwrap();
        assert_eq!((download.range.clone(), download.total), (3..10, Some(10)));
        assert_eq!(read(download).unwrap(), b"3456789");
        // A 200 shorter than the prefix to skip.
        let short = Served {
            declared: None,
            ..ignored
        };
        assert!(matches!(
            ranged_body(body(b"01"), Some(5..8), short.clone()),
            Err(Error::InvalidResponse("short body"))
        ));
        // A 206 that covers the range.
        let partial = Served {
            partial: true,
            content_range: Some((2..5, Some(10))),
            declared: Some(3),
        };
        let download = ranged_body(body(b"234"), Some(2..5), partial.clone()).unwrap();
        assert_eq!((download.range.clone(), download.total), (2..5, Some(10)));
        assert_eq!(read(download).unwrap(), b"234");
        // A 206 at the wrong place, or one without its Content-Range.
        for content_range in [Some((1..5, Some(10))), Some((2..4, Some(10))), None] {
            let served = Served {
                content_range,
                ..partial.clone()
            };
            assert!(matches!(
                ranged_body(body(b"234"), Some(2..5), served),
                Err(Error::InvalidResponse("content-range"))
            ));
        }
        // A body cut off after the headers fails while reading.
        let cut = ranged_body(body(b"23"), Some(2..5), partial).unwrap();
        assert_eq!(read(cut).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        let cut = ranged_body(body(b"01234"), Some(3..8), short).unwrap();
        assert_eq!(read(cut).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn whole_bodies_are_exact_when_their_length_is_declared() {
        let declared = Served {
            partial: false,
            content_range: None,
            declared: Some(4),
        };
        let download = ranged_body(body(b"abcd"), None, declared.clone()).unwrap();
        assert_eq!((download.range.clone(), download.total), (0..4, Some(4)));
        assert_eq!(read(download).unwrap(), b"abcd");
        let cut = ranged_body(body(b"ab"), None, declared).unwrap();
        assert_eq!(read(cut).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        let streamed = ranged_body(body(b"abc"), None, Served::default()).unwrap();
        assert_eq!(
            (streamed.range.clone(), streamed.total),
            (0..u64::MAX, None)
        );
        assert_eq!(read(streamed).unwrap(), b"abc");
    }

    #[test]
    fn timestamps_parse_strictly() {
        let epoch = parse_rfc3339("1970-01-01T00:00:00Z").unwrap();
        assert_eq!(epoch, SystemTime::UNIX_EPOCH);
        let leap = parse_rfc3339(" 2024-02-29T23:59:59.999999999Z ").unwrap();
        let since = leap.duration_since(SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(since.as_secs(), 1_709_251_199);
        assert_eq!(since.subsec_nanos(), 999_999_999);
        assert_eq!(
            parse_rfc3339("2024-01-03T03:04:05+01:00"),
            parse_rfc3339("2024-01-03T02:04:05Z")
        );
        assert!(parse_rfc3339("1969-12-31T23:59:59Z").unwrap() < SystemTime::UNIX_EPOCH);
        assert!(parse_rfc3339("2024-13-01T00:00:00Z").is_none());
        assert!(parse_rfc3339("2024-01-01T00:00:00").is_none());
        assert!(parse_rfc3339("2015-05-12 15:50:38").is_none());
        assert!(parse_rfc3339("yesterday").is_none());
        let date = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(
            date.duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            784_111_777
        );
        assert!(parse_http_date("yesterday").is_none());
    }
}
