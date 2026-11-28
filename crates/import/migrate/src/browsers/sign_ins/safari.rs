//! Safari's cookies, from the `Cookies.binarycookies` file a Mac keeps them in: pages of records,
//! each naming its domain, name, path and value and when it expires. Safari seals none of them,
//! so a home read from its disk brings them over as the Mac had them.
//!
//! The file is read within a byte bound, and every offset in it is checked against the record
//! it is in: a damaged page is left out, never read past.

use std::path::Path;

use browser_continuity::sessions::{Cookie, SameSite};
use zeroize::Zeroizing;

use super::{MAX_COOKIE_DATABASE_BYTES, MAX_READ, scoped};
use crate::browsers::bounded_read;
use crate::thunderbird::LoginsError;

/// Where a Mac keeps Safari's cookies, under its home's `Library`: in Safari's container since
/// macOS 10.14, and beside the other apps' before.
const FILES: [&str; 2] = [
    "Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies",
    "Cookies/Cookies.binarycookies",
];
/// Seconds between the Unix epoch and 2001-01-01, where a Mac counts its times from.
const MAC_EPOCH_OFFSET: f64 = 978_307_200.0;
/// A record's flag for a cookie sent over HTTPS alone.
const SECURE: u32 = 1;
/// A record's flag for a cookie no page script reads.
const HTTP_ONLY: u32 = 4;

/// Whether Safari, whose directory is `safari` (a home's `Library/Safari`), kept any cookies.
pub(crate) fn keeps_cookies(safari: &Path) -> bool {
    safari
        .parent()
        .is_some_and(|library| FILES.iter().any(|file| library.join(file).is_file()))
}

/// The cookies Safari, whose directory is `safari` (a home's `Library/Safari`), keeps, at `now`
/// (Unix seconds): expired ones are left out.
pub(super) fn cookies(safari: &Path, now: i64) -> Result<Vec<Cookie>, LoginsError> {
    let Some(library) = safari.parent() else {
        return Err(LoginsError::NoneSaved);
    };
    let Some(bytes) = FILES.iter().find_map(|file| {
        match bounded_read(&library.join(file), MAX_COOKIE_DATABASE_BYTES) {
            Ok(bytes) => Some(Ok(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => Some(Err(LoginsError::Unreadable(format!(
                "Cookies.binarycookies: {error}"
            )))),
        }
    }) else {
        return Err(LoginsError::NoneSaved);
    };
    let bytes = Zeroizing::new(bytes?);
    let pages = pages(&bytes).ok_or_else(|| {
        LoginsError::Unreadable("Cookies.binarycookies: not Safari's cookie file".to_owned())
    })?;
    let mut found = Vec::new();
    for page in pages {
        for record in records(page) {
            if found.len() == MAX_READ {
                return Ok(found);
            }
            if let Some(cookie) = cookie(record, now) {
                found.push(cookie);
            }
        }
    }
    Ok(found)
}

fn u32_be(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u32_le(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn f64_le(bytes: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// The file's pages: after `cook`, how many pages there are and each one's size, all
/// big-endian, then the pages one after another.
fn pages(bytes: &[u8]) -> Option<Vec<&[u8]>> {
    if bytes.get(..4)? != b"cook" {
        return None;
    }
    let count = usize::try_from(u32_be(bytes, 4)?).ok()?;
    let mut at = 8_usize.checked_add(count.checked_mul(4)?)?;
    let mut pages = Vec::new();
    for index in 0..count {
        let size =
            usize::try_from(u32_be(bytes, 8_usize.checked_add(index.checked_mul(4)?)?)?).ok()?;
        let end = at.checked_add(size)?;
        pages.push(bytes.get(at..end)?);
        at = end;
    }
    Some(pages)
}

/// A page's records: after its `00 00 01 00` header, how many records it holds and where each
/// begins in the page, little-endian; each record's first word is its size. A page whose header
/// or offsets do not read gives none.
fn records(page: &[u8]) -> Vec<&[u8]> {
    let read = || -> Option<Vec<&[u8]>> {
        if page.get(..4)? != [0, 0, 1, 0] {
            return None;
        }
        let count = usize::try_from(u32_le(page, 4)?).ok()?;
        let mut records = Vec::new();
        for index in 0..count {
            let start =
                usize::try_from(u32_le(page, 8_usize.checked_add(index.checked_mul(4)?)?)?).ok()?;
            let size = usize::try_from(u32_le(page, start)?).ok()?;
            records.push(page.get(start..start.checked_add(size)?)?);
        }
        Some(records)
    };
    read().unwrap_or_default()
}

/// The text a record holds at `offset`, up to its terminating zero.
fn text_at(record: &[u8], offset: u32) -> Option<String> {
    let rest = record.get(usize::try_from(offset).ok()?..)?;
    let end = rest.iter().position(|byte| *byte == 0)?;
    std::str::from_utf8(rest.get(..end)?)
        .ok()
        .map(str::to_owned)
}

/// One record as a cookie: its flags at 8, the offsets of its domain, name, path and value at 16
/// to 28 (from the record's start), and when it expires at 40, as seconds from 2001. An expired
/// one, or one whose fields do not read, is none.
fn cookie(record: &[u8], now: i64) -> Option<Cookie> {
    let flags = u32_le(record, 8)?;
    let domain = text_at(record, u32_le(record, 16)?)?;
    let name = text_at(record, u32_le(record, 20)?)?;
    let path = text_at(record, u32_le(record, 24)?)?;
    let value = Zeroizing::new(text_at(record, u32_le(record, 28)?)?);
    let expires = f64_le(record, 40)?;
    if !expires.is_finite() {
        return None;
    }
    // A whole number of seconds well within an i64: a cookie's lifetime is at most centuries.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a finite expiry is rounded to the second it falls in"
    )]
    let expires = (expires + MAC_EPOCH_OFFSET).floor().clamp(0.0, 1e15) as i64;
    if expires <= now {
        return None;
    }
    let (host, domain, host_only) = scoped(&domain);
    let secure = flags & SECURE != 0;
    Some(Cookie {
        host,
        domain,
        host_only,
        name,
        value: value.to_string(),
        path,
        expires_unix_seconds: Some(expires),
        secure,
        http_only: flags & HTTP_ONLY != 0,
        same_site: SameSite::Unspecified,
    })
}
