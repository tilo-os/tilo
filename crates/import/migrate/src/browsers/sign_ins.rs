//! The sites a browser profile is signed in to: the cookies its own cookie store keeps, read from
//! Firefox's `cookies.sqlite`, from a Chromium-family browser's `Cookies` with its `OSCrypt`
//! key, as the profile's saved passwords are ([`super::saved_logins`]), and from Safari's
//! `Cookies.binarycookies` on a Mac's disk ([`safari`]).
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
        Engine::Safari => safari::cookies(&profile.directory, now)?,
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

/// An account a profile was signed in to that the workspace connects through the account's own
/// sign-in: its cookies are never what connects it, as a browser's cookies are no grant a
/// workspace may hold for mail, calendars and files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Account {
    Google,
    Microsoft,
    Spotify,
    /// Discord, whose sign-in a page keeps as a token in its own storage, not a cookie
    /// ([`discord_signed_in`]). The token is never read: Discord lets no program act with a
    /// person's own token, so the workspace's Discord connects through Discord's own sign-in.
    Discord,
}

impl Account {
    const ALL: [Self; 3] = [Self::Google, Self::Microsoft, Self::Spotify];

    /// The provider the workspace's accounts sign in with: `google`, `microsoft`, `spotify`,
    /// `discord`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Microsoft => "microsoft",
            Self::Spotify => "spotify",
            Self::Discord => "discord",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::Microsoft => "Microsoft",
            Self::Spotify => "Spotify",
            Self::Discord => "Discord",
        }
    }

    /// The cookies only a signed-in visit leaves: `(domain, names)`. A visit signed out leaves
    /// others of the same sites, which say nothing of an account.
    fn signed_in_cookies(self) -> &'static [(&'static str, &'static [&'static str])] {
        match self {
            Self::Google => &[("google.com", &["SID", "__Secure-1PSID", "__Secure-3PSID"])],
            Self::Microsoft => &[
                (
                    "login.live.com",
                    &["__Host-MSAAUTH", "__Host-MSAAUTHP", "MSPAuth", "WLSSC"],
                ),
                (
                    "login.microsoftonline.com",
                    &["ESTSAUTH", "ESTSAUTHPERSISTENT"],
                ),
            ],
            Self::Spotify => &[("spotify.com", &["sp_dc"])],
            Self::Discord => &[],
        }
    }

    /// The sign-in pages a saved password for the account is kept for.
    fn sign_in_hosts(self) -> &'static [&'static str] {
        match self {
            Self::Google => &["accounts.google.com"],
            Self::Microsoft => &["login.live.com", "login.microsoftonline.com"],
            Self::Spotify => &["accounts.spotify.com"],
            Self::Discord => &["discord.com"],
        }
    }
}

/// The accounts `cookies` show their profile signed in to, each once, in [`Account`]'s order.
#[must_use]
pub fn accounts(cookies: &[Cookie]) -> Vec<Account> {
    Account::ALL
        .into_iter()
        .filter(|account| {
            cookies.iter().any(|cookie| {
                account.signed_in_cookies().iter().any(|(domain, names)| {
                    within(&cookie.domain, domain) && names.contains(&cookie.name.as_str())
                })
            })
        })
        .collect()
}

/// The address `account` was signed in with, as a password the profile saved for its sign-in
/// page names it: the one that reads as an address, where several do.
#[must_use]
pub fn address_of(account: Account, logins: &[crate::thunderbird::SavedLogin]) -> Option<String> {
    let names: Vec<&str> = logins
        .iter()
        .filter(|login| {
            url::Url::parse(&login.origin).is_ok_and(|origin| {
                origin
                    .host_str()
                    .is_some_and(|host| account.sign_in_hosts().contains(&host))
            })
        })
        .map(|login| login.username.trim())
        .filter(|name| !name.is_empty())
        .collect();
    names
        .iter()
        .find(|name| name.contains('@'))
        .or_else(|| names.first())
        .map(|name| (*name).to_owned())
}

/// Where Discord's page keeps its sign-in token, as the key its storage names it by: a
/// Chromium-family browser's `Local Storage` writes `_<origin>`, a zero, a one and the key.
const DISCORD_TOKEN_KEY: &[u8] = b"_https://discord.com\x00\x01token";
/// The largest storage file looked through for it.
const MAX_STORAGE_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// The most storage files looked through for it.
const MAX_STORAGE_FILES: usize = 64;

/// Whether `profile` is signed in to Discord: whether Discord's page kept its sign-in token in
/// the profile's storage. Only that the key is there is read, never what it holds.
#[must_use]
pub fn discord_signed_in(profile: &BrowserProfile) -> bool {
    match profile.engine {
        Engine::Firefox => {
            let Ok(Some((_directory, path))) = copied(
                &profile.directory,
                &["storage/default/https+++discord.com/ls/data.sqlite"],
            ) else {
                return false;
            };
            open(&path).is_ok_and(|connection| {
                connection
                    .query_row("SELECT count(*) FROM data WHERE key = 'token'", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .is_ok_and(|count| count > 0)
            })
        }
        Engine::Chromium => {
            let Ok(entries) = std::fs::read_dir(profile.directory.join("Local Storage/leveldb"))
            else {
                return false;
            };
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "log" || extension == "ldb")
                })
                .take(MAX_STORAGE_FILES)
                .any(|path| {
                    // The file holds what the pages keep, a token among it: wiped once looked
                    // through.
                    bounded_read(&path, MAX_STORAGE_FILE_BYTES)
                        .map(Zeroizing::new)
                        .is_ok_and(|bytes| {
                            bytes
                                .windows(DISCORD_TOKEN_KEY.len())
                                .any(|window| window == DISCORD_TOKEN_KEY)
                        })
                })
        }
        Engine::Safari => false,
    }
}

/// Whether `domain` is `base` or one of its subdomains.
fn within(domain: &str, base: &str) -> bool {
    domain == base
        || domain
            .strip_suffix(base)
            .is_some_and(|prefix| prefix.ends_with('.'))
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

pub(crate) mod safari;
#[cfg(test)]
pub(crate) mod tests;
