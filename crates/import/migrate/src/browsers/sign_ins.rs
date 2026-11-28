//! The sites a browser profile is signed in to: the cookies its own cookie store keeps, read from
//! Firefox's `cookies.sqlite` and from a Chromium-family browser's `Cookies` with its `OSCrypt`
//! key, as the profile's saved passwords are ([`super::saved_logins`]).
//!
//! A cookie is a credential. It is held only in memory that wipes itself
//! ([`browser_continuity::sessions::Cookie`]) and handed straight to the workspace's vault; it is
//! never written, printed or put in a resource. Only what a site set for itself in the profile
//! the person browses with is read: a Firefox container's cookies, a private window's and a
//! Chromium cookie partitioned under another site stay where they are, as do expired ones.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;

use browser_continuity::sessions::{Cookie, MAX_COOKIES, MAX_SESSION_BYTES, SameSite, Service};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use super::{BrowserProfile, Engine, bounded_read, keyring_key};
use crate::thunderbird::LoginsError;

/// The largest cookie database read.
const MAX_COOKIE_DATABASE_BYTES: u64 = 256 * 1024 * 1024;
/// The most cookies one profile is read for.
const MAX_READ: usize = 200_000;
/// Seconds between 1601-01-01, where Chromium counts its times from, and the Unix epoch.
const WINDOWS_EPOCH_OFFSET: i64 = 11_644_473_600;
/// The bytes one session's cookies may take, leaving room for what the session says besides.
const SESSION_BUDGET: usize = MAX_SESSION_BYTES - 64 * 1024;

/// The cookies `profile` keeps for the sites it is signed in to, each one Tilo's Browser takes,
/// at `now` (Unix seconds): expired ones are left out.
///
/// # Errors
/// See [`LoginsError`]: no cookie store, a keyring that keeps the key locked, or unreadable.
pub fn sign_ins(profile: &BrowserProfile, now: i64) -> Result<Vec<Cookie>, LoginsError> {
    let read = match profile.engine {
        Engine::Firefox => firefox(profile, now)?,
        Engine::Chromium => chromium(profile, now)?,
    };
    let mut seen = BTreeSet::new();
    let mut cookies: Vec<Cookie> = read
        .into_iter()
        .filter(|cookie| cookie.validate(Service::Web).is_ok())
        .filter(|cookie| {
            seen.insert((
                cookie.domain.clone(),
                cookie.host_only,
                cookie.path.clone(),
                cookie.name.clone(),
            ))
        })
        .collect();
    if cookies.is_empty() {
        return Err(LoginsError::NoneSaved);
    }
    cookies.sort_by(|a, b| {
        (&a.domain, &a.host, &a.path, &a.name).cmp(&(&b.domain, &b.host, &b.path, &b.name))
    });
    Ok(cookies)
}

/// How many sites `cookies` sign in to: the sites their domains belong to, by their last two
/// labels, which is what a person counts.
#[must_use]
pub fn site_count(cookies: &[Cookie]) -> usize {
    cookies
        .iter()
        .map(|cookie| {
            let mut labels: Vec<&str> = cookie.domain.rsplit('.').take(2).collect();
            labels.reverse();
            labels.join(".")
        })
        .collect::<BTreeSet<_>>()
        .len()
}

/// The sessions `cookies` are kept as: one for each service Tilo opens in a window of its own
/// (Slack, Discord) with that service's cookies, then every cookie in as few sessions for the
/// person's own Browser window as each session's bounds allow.
#[must_use]
pub fn sessions(cookies: &[Cookie]) -> Vec<(Service, Vec<Cookie>)> {
    let mut sessions = Vec::new();
    for service in [Service::Slack, Service::Discord] {
        let theirs: Vec<Cookie> = cookies
            .iter()
            .filter(|cookie| Service::of_host(&cookie.domain) == Some(service))
            .take(MAX_COOKIES)
            .cloned()
            .collect();
        if !theirs.is_empty() {
            sessions.push((service, theirs));
        }
    }
    let mut part: Vec<Cookie> = Vec::new();
    let mut bytes = 0_usize;
    for cookie in cookies {
        let size = [
            &cookie.host,
            &cookie.domain,
            &cookie.path,
            &cookie.name,
            &cookie.value,
        ]
        .iter()
        .map(|text| text.len())
        .sum::<usize>();
        if !part.is_empty()
            && (part.len() == MAX_COOKIES || bytes.saturating_add(size) > SESSION_BUDGET)
        {
            sessions.push((Service::Web, std::mem::take(&mut part)));
            bytes = 0;
        }
        bytes = bytes.saturating_add(size);
        part.push(cookie.clone());
    }
    if !part.is_empty() {
        sessions.push((Service::Web, part));
    }
    sessions
}

/// A copy of the database `name` in `profile` (and its write-ahead log beside it), which reads
/// as it was while the browser holds the original open. `None` when there is none.
fn copied(
    profile: &Path,
    names: &[&str],
) -> Result<Option<(tempfile::TempDir, std::path::PathBuf)>, LoginsError> {
    let unreadable = |what: String| LoginsError::Unreadable(format!("cookies: {what}"));
    let Some(name) = names
        .iter()
        .find(|name| profile.join(name).is_file())
        .copied()
    else {
        return Ok(None);
    };
    let directory = tempfile::Builder::new()
        .prefix("tilo-browser-cookies-")
        .tempdir()
        .map_err(|error| unreadable(error.to_string()))?;
    let base = Path::new(name)
        .file_name()
        .ok_or_else(|| unreadable("no name".to_owned()))?;
    let into = directory.path().join(base);
    for suffix in ["", "-wal"] {
        let from = profile.join(format!("{name}{suffix}"));
        let bytes = match bounded_read(&from, MAX_COOKIE_DATABASE_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if !suffix.is_empty() && error.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => return Err(unreadable(error.to_string())),
        };
        let mut file = std::fs::File::create(
            directory
                .path()
                .join(format!("{}{suffix}", base.to_string_lossy())),
        )
        .map_err(|error| unreadable(error.to_string()))?;
        file.write_all(&bytes)
            .and_then(|()| file.flush())
            .map_err(|error| unreadable(error.to_string()))?;
    }
    Ok(Some((directory, into)))
}

fn open(path: &Path) -> Result<rusqlite::Connection, LoginsError> {
    rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| LoginsError::Unreadable("cookies: not a database".to_owned()))
}

/// The host and domain a cookie stored under `host` (`.example.com` for one every subdomain
/// gets) is for, and whether it is for that host alone.
fn scoped(host: &str) -> (String, String, bool) {
    let host = host.to_ascii_lowercase();
    match host.strip_prefix('.') {
        Some(domain) => (domain.to_owned(), domain.to_owned(), false),
        None => (host.clone(), host, true),
    }
}

/// What a cookie with `same_site` keeps, where `SameSite=None` stands only for a secure one: an
/// insecure one says nothing, as its browser treats it.
fn same_site(value: i64, secure: bool) -> SameSite {
    match value {
        0 if secure => SameSite::None,
        1 => SameSite::Lax,
        2 => SameSite::Strict,
        _ => SameSite::Unspecified,
    }
}

/// Firefox's cookies, in the profile the person browses with: a container's, or one a private
/// window set, stay.
fn firefox(profile: &BrowserProfile, now: i64) -> Result<Vec<Cookie>, LoginsError> {
    let Some((_directory, path)) = copied(&profile.directory, &["cookies.sqlite"])? else {
        return Err(LoginsError::NoneSaved);
    };
    let connection = open(&path)?;
    let unreadable = || LoginsError::Unreadable("cookies.sqlite: no cookies table".to_owned());
    let mut statement = connection
        .prepare(
            "SELECT host, name, value, path, expiry, isSecure, isHttpOnly, sameSite, \
             originAttributes FROM moz_cookies",
        )
        .map_err(|_| unreadable())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                Zeroizing::new(row.get::<_, String>(2)?),
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, Option<i64>>(7)?.unwrap_or(0),
                row.get::<_, Option<String>>(8)?.unwrap_or_default(),
            ))
        })
        .map_err(|_| unreadable())?;
    let mut found = Vec::new();
    for (host, name, value, path, expiry, secure, http_only, same, attributes) in
        rows.filter_map(Result::ok).take(MAX_READ)
    {
        if !attributes.is_empty() {
            continue;
        }
        // Newer Firefox counts a cookie's expiry in milliseconds, older in seconds.
        let expires = if expiry > 100_000_000_000 {
            expiry / 1000
        } else {
            expiry
        };
        if expires <= now {
            continue;
        }
        let (host, domain, host_only) = scoped(&host);
        found.push(Cookie {
            host,
            domain,
            host_only,
            name,
            value: value.to_string(),
            path,
            expires_unix_seconds: Some(expires),
            secure,
            http_only,
            same_site: same_site(same, secure),
        });
    }
    Ok(found)
}

/// The value `sealed` (with its `v10`/`v11` prefix) holds for `host_key`, opened with `key`:
/// from cookie database version 24 the value is sealed behind the hash of its host, which must
/// be that host's. `None` where it does not open.
fn unsealed(
    key: &Zeroizing<[u8; 16]>,
    sealed: &[u8],
    host_key: &str,
    version: i64,
) -> Option<Zeroizing<String>> {
    let opened = tilo_seal::compat::chromium_oscrypt_plaintext(key, sealed.get(3..)?).ok()?;
    let opened: &[u8] = if version >= 24 {
        let (hash, rest) = opened.split_at_checked(32)?;
        (hash == Sha256::digest(host_key.as_bytes()).as_slice()).then_some(rest)?
    } else {
        &opened
    };
    std::str::from_utf8(opened)
        .ok()
        .map(|text| Zeroizing::new(text.to_owned()))
}

/// A Chromium-family profile's cookies, each value decrypted with its `OSCrypt` key: `v10` with
/// Chromium's fixed one, `v11` with the one the system keyring keeps for the brand.
fn chromium(profile: &BrowserProfile, now: i64) -> Result<Vec<Cookie>, LoginsError> {
    let Some((_directory, path)) = copied(&profile.directory, &["Network/Cookies", "Cookies"])?
    else {
        return Err(LoginsError::NoneSaved);
    };
    let connection = open(&path)?;
    let unreadable = || LoginsError::Unreadable("Cookies: no cookies table".to_owned());
    // From version 24 a value is sealed behind the hash of the host it is for.
    let version: i64 = connection
        .query_row("SELECT value FROM meta WHERE key = 'version'", [], |row| {
            row.get::<_, String>(0)
        })
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let partitioned = connection
        .prepare("SELECT top_frame_site_key FROM cookies LIMIT 0")
        .is_ok();
    let query = format!(
        "SELECT host_key, name, value, encrypted_value, path, expires_utc, is_secure, \
         is_httponly, samesite, {} FROM cookies",
        if partitioned {
            "top_frame_site_key"
        } else {
            "''"
        }
    );
    let mut statement = connection.prepare(&query).map_err(|_| unreadable())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                Zeroizing::new(row.get::<_, String>(2)?),
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, bool>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, String>(9)?,
            ))
        })
        .map_err(|_| unreadable())?;
    let v10 = tilo_seal::compat::chromium_oscrypt_key(b"peanuts");
    let mut v11: Option<Option<Zeroizing<[u8; 16]>>> = None;
    let mut found = Vec::new();
    let mut locked = false;
    for (host_key, name, plain, sealed, path, expires, secure, http_only, same, partition) in
        rows.filter_map(Result::ok).take(MAX_READ)
    {
        if !partition.is_empty() {
            continue;
        }
        // Chromium counts in microseconds from 1601; zero is a cookie for the session.
        let expires =
            (expires != 0).then(|| (expires / 1_000_000).saturating_sub(WINDOWS_EPOCH_OFFSET));
        if expires.is_some_and(|at| at <= now) {
            continue;
        }
        let value = if sealed.is_empty() {
            plain
        } else {
            let key = match sealed.get(..3) {
                Some(b"v10") => v10.clone(),
                Some(b"v11") => {
                    let key = v11.get_or_insert_with(|| keyring_key(profile));
                    let Some(key) = key.clone() else {
                        locked = true;
                        continue;
                    };
                    key
                }
                _ => continue,
            };
            let Some(text) = unsealed(&key, &sealed, &host_key, version) else {
                continue;
            };
            text
        };
        let (host, domain, host_only) = scoped(&host_key);
        found.push(Cookie {
            host,
            domain,
            host_only,
            name,
            value: value.to_string(),
            path,
            expires_unix_seconds: expires,
            secure,
            http_only,
            same_site: same_site(same, secure),
        });
    }
    if found.is_empty() && locked {
        return Err(LoginsError::Locked);
    }
    Ok(found)
}

#[cfg(test)]
pub(crate) mod tests;
