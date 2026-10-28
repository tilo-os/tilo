//! Read-only cookie extraction from one explicitly granted browser profile.
//! Only fixed cookie stores are opened; values never enter diagnostics.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use browser_continuity::Provider;
use browser_continuity::sessions::{Cookie, MAX_COOKIES, MAX_SESSION_BYTES, SameSite, Service};
use rusqlite::{Connection, OpenFlags};
use tilo_files::native::atomic::{Privacy, StorageDirectory};
use zeroize::Zeroizing;

#[cfg(target_os = "linux")]
pub mod chromium_linux;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod discord_origin;
#[cfg(target_os = "linux")]
mod slack_asar;
#[cfg(target_os = "linux")]
pub mod slack_linux;
#[cfg(target_os = "linux")]
pub mod slack_locator;
#[cfg(target_os = "linux")]
mod slack_modules;

/// Private native adapter, bound to the exact owner-selected profile directory.
/// Implementations must authenticate schema 24's raw host hash before returning
/// the value, and must never expose keys or values through diagnostics.
pub trait ChromiumDecryptor: Send + Sync {
    fn approved_profile(&self) -> (u32, u64, u64);
    /// # Errors
    /// Refuses unavailable keys, invalid ciphertext or host binding, and unknown formats.
    fn decrypt(
        &self,
        host_key: &str,
        schema: i64,
        encrypted: &[u8],
    ) -> Result<Zeroizing<String>, SourceError>;
}

const MAX_DATABASE_BYTES: u64 = 64 * 1024 * 1024;
const CHROMIUM_EPOCH_MICROS: i64 = 11_644_473_600_000_000;

/// The caller supplies the exact owner-approved profile handle, never a path
/// discovered by this reader or permission inferred from a running browser.
pub struct SourceGrant {
    profile: File,
    provider: Provider,
    decryptor: Option<std::sync::Arc<dyn ChromiumDecryptor>>,
}
impl SourceGrant {
    /// # Errors
    /// Refuses foreign or non-directory grants without modifying permissions.
    pub fn new(profile: File, provider: Provider) -> Result<Self, SourceError> {
        let directory = StorageDirectory::from_file(profile.try_clone().map_err(unavailable)?)
            .map_err(unavailable)?;
        directory
            .validate_namespace_control()
            .map_err(unavailable)?;
        Ok(Self {
            profile,
            provider,
            decryptor: None,
        })
    }

    /// Attach only after the owner explicitly selects this browser's sessions.
    ///
    /// # Errors
    /// Refuses adapters bound to any other owner or profile, or a non-Chromium source.
    pub fn with_chromium_decryptor(
        mut self,
        decryptor: std::sync::Arc<dyn ChromiumDecryptor>,
    ) -> Result<Self, SourceError> {
        let facts = self.profile.metadata().map_err(unavailable)?;
        if self.provider != Provider::Chromium
            || decryptor.approved_profile() != (facts.uid(), facts.dev(), facts.ino())
        {
            return Err(SourceError::Unavailable);
        }
        self.decryptor = Some(decryptor);
        Ok(self)
    }
}

/// Source bytes have no fabricated destination identity or setup authority.
pub struct CapturedCookies {
    pub cookies: Vec<Cookie>,
    pub source_snapshot_digest: [u8; 32],
}
impl std::fmt::Debug for CapturedCookies {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CapturedCookies { private selected session }")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceError {
    Unavailable,
    Changed,
    UnsupportedSchema,
    UnsupportedEncryption,
    UnsupportedPartition,
    InvalidCookies,
    LimitExceeded,
}
impl std::fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "Selected browser cookie store is unavailable",
            Self::Changed => "Selected browser cookie store changed; close the browser and retry",
            Self::UnsupportedSchema => "Selected browser cookie format is unsupported",
            Self::UnsupportedEncryption => {
                "Selected browser session requires an unavailable decryption adapter"
            }
            Self::UnsupportedPartition => "Selected browser session uses unsupported partitioning",
            Self::InvalidCookies => {
                "Selected browser session contains unsupported cookie attributes"
            }
            Self::LimitExceeded => "Selected browser session exceeds migration limits",
        })
    }
}
impl std::error::Error for SourceError {}
fn unavailable(_: std::io::Error) -> SourceError {
    SourceError::Unavailable
}
fn sqlite(_: rusqlite::Error) -> SourceError {
    SourceError::UnsupportedSchema
}

#[derive(PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn of(file: &File) -> Result<Self, SourceError> {
        let metadata = file.metadata().map_err(unavailable)?;
        if !metadata.is_file() {
            return Err(SourceError::Unavailable);
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            mode: metadata.mode(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}
#[derive(PartialEq, Eq)]
struct Capture {
    stamp: Stamp,
    bytes: Zeroizing<Vec<u8>>,
}
fn capture(directory: &StorageDirectory, owner: u32, name: &str) -> Result<Capture, SourceError> {
    directory
        .validate_namespace_control()
        .map_err(unavailable)?;
    let file = directory
        .open_read(OsStr::new(name), Privacy::Inherit)
        .map_err(unavailable)?;
    let stamp = Stamp::of(&file)?;
    if stamp.owner != owner {
        return Err(SourceError::Unavailable);
    }
    if stamp.length > MAX_DATABASE_BYTES {
        return Err(SourceError::LimitExceeded);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_DATABASE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(unavailable)?;
    let current = directory
        .open_read(OsStr::new(name), Privacy::Inherit)
        .map_err(unavailable)?;
    if bytes.len() as u64 != stamp.length || Stamp::of(&current)? != stamp {
        return Err(SourceError::Changed);
    }
    Ok(Capture { stamp, bytes })
}
fn wal(
    directory: &StorageDirectory,
    owner: u32,
    name: &str,
) -> Result<Option<Capture>, SourceError> {
    match directory.open_read(OsStr::new(name), Privacy::Inherit) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(unavailable(error)),
        Ok(_) => capture(directory, owner, name).map(Some),
    }
}

fn source_directory(
    grant: &SourceGrant,
) -> Result<(StorageDirectory, &'static str, u32), SourceError> {
    let owner = grant.profile.metadata().map_err(unavailable)?.uid();
    let root = StorageDirectory::from_file(grant.profile.try_clone().map_err(unavailable)?)
        .map_err(unavailable)?;
    root.validate_namespace_control().map_err(unavailable)?;
    if grant.provider == Provider::Firefox {
        return Ok((root, "cookies.sqlite", owner));
    }
    match super::super::nofollow::metadata_nofollow(&grant.profile, Path::new("Network")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(unavailable(error)),
        Ok(metadata) => {
            let held = super::super::nofollow::open_directory_nofollow(
                &grant.profile,
                Path::new("Network"),
                &metadata,
            )
            .map_err(unavailable)?;
            let held = super::super::migration_durability::readable_directory(&held)
                .map_err(unavailable)?;
            let network = StorageDirectory::from_file(held).map_err(unavailable)?;
            network.validate_namespace_control().map_err(unavailable)?;
            match network.open_read(OsStr::new("Cookies"), Privacy::Inherit) {
                Ok(_) => return Ok((network, "Cookies", owner)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(unavailable(error)),
            }
        }
    }
    Ok((root, "Cookies", owner))
}

fn snapshot(
    grant: &SourceGrant,
    service: Service,
) -> Result<(tempfile::TempDir, Connection, [u8; 32]), SourceError> {
    let (directory, name, owner) = source_directory(grant)?;
    let wal_name = format!("{name}-wal");
    let database = capture(&directory, owner, name)?;
    let journal = wal(&directory, owner, &wal_name)?;
    if database != capture(&directory, owner, name)?
        || journal != wal(&directory, owner, &wal_name)?
    {
        return Err(SourceError::Changed);
    }
    // Reattest every name after the complete second capture, including absence.
    let current = directory
        .open_read(OsStr::new(name), Privacy::Inherit)
        .map_err(unavailable)?;
    if Stamp::of(&current)? != database.stamp {
        return Err(SourceError::Changed);
    }
    match (
        journal.as_ref(),
        directory.open_read(OsStr::new(&wal_name), Privacy::Inherit),
    ) {
        (None, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        (Some(expected), Ok(file)) if Stamp::of(&file)? == expected.stamp => {}
        _ => return Err(SourceError::Changed),
    }
    directory
        .validate_namespace_control()
        .map_err(unavailable)?;
    let mut digest = blake3::Hasher::new();
    digest.update(b"tilo-selected-service-cookie-source-v1");
    digest.update(service.as_str().as_bytes());
    digest.update(name.as_bytes());
    digest.update(&(database.bytes.len() as u64).to_le_bytes());
    digest.update(&database.bytes);
    digest.update(&[u8::from(journal.is_some())]);
    if let Some(journal) = &journal {
        digest.update(&journal.bytes);
    }
    let temporary = tempfile::tempdir().map_err(unavailable)?;
    let path = temporary.path().join(name);
    private_write(&path, &database.bytes)?;
    if let Some(journal) = journal {
        private_write(&temporary.path().join(wal_name), &journal.bytes)?;
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite)?;
    connection
        .execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; BEGIN;")
        .map_err(sqlite)?;
    let check: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(sqlite)?;
    if check != "ok" {
        return Err(SourceError::UnsupportedSchema);
    }
    Ok((temporary, connection, *digest.finalize().as_bytes()))
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<(), SourceError> {
    let mut file = tilo_files::native::private::create_new(path).map_err(unavailable)?;
    file.write_all(bytes).map_err(unavailable)
}

fn boolean(value: i64) -> Result<bool, SourceError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(SourceError::InvalidCookies),
    }
}
fn same_site(value: i64, firefox: bool) -> Result<SameSite, SourceError> {
    match value {
        -1 if !firefox => Ok(SameSite::Unspecified),
        256 if firefox => Ok(SameSite::Unspecified),
        0 => Ok(SameSite::None),
        1 => Ok(SameSite::Lax),
        2 => Ok(SameSite::Strict),
        _ => Err(SourceError::InvalidCookies),
    }
}
fn host(raw: &str) -> (String, bool) {
    let host_only = !raw.starts_with('.');
    (
        raw.strip_prefix('.').unwrap_or(raw).to_ascii_lowercase(),
        host_only,
    )
}

/// Bounded, value-free counts for one selected service's cookie store.
/// Counting never decrypts a cookie and never establishes an account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedCounts {
    pub cookies: u64,
    pub origins: u64,
}

/// Count matching cookie rows and distinct hosts without reading values.
/// The source is copied consistently and only the private copy is queried.
///
/// # Errors
/// Refuses unavailable/changed sources or an unsupported cookie schema.
pub fn count_selected(
    grant: &SourceGrant,
    service: Service,
) -> Result<SelectedCounts, SourceError> {
    let (_temporary, connection, _source_snapshot_digest) = snapshot(grant, service)?;
    let query = match grant.provider {
        Provider::Firefox => {
            let version: i64 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .map_err(sqlite)?;
            if !matches!(version, 15..=17) {
                return Err(SourceError::UnsupportedSchema);
            }
            match service {
                Service::Slack => {
                    "SELECT COUNT(*), COUNT(DISTINCT host) FROM moz_cookies WHERE lower(host)='slack.com' OR lower(host) LIKE '%.slack.com'"
                }
                Service::Discord => {
                    "SELECT COUNT(*), COUNT(DISTINCT host) FROM moz_cookies WHERE lower(host)='discord.com' OR lower(host) LIKE '%.discord.com'"
                }
            }
        }
        Provider::Chromium => {
            let version = connection
                .query_row("SELECT value FROM meta WHERE key='version'", [], |row| {
                    Ok(match row.get_ref(0)? {
                        rusqlite::types::ValueRef::Integer(version @ (23 | 24)) => Some(version),
                        rusqlite::types::ValueRef::Text(b"23") => Some(23),
                        rusqlite::types::ValueRef::Text(b"24") => Some(24),
                        _ => None,
                    })
                })
                .map_err(sqlite)?;
            if version.is_none() {
                return Err(SourceError::UnsupportedSchema);
            }
            match service {
                Service::Slack => {
                    "SELECT COUNT(*), COUNT(DISTINCT host_key) FROM cookies WHERE lower(host_key)='slack.com' OR lower(host_key) LIKE '%.slack.com'"
                }
                Service::Discord => {
                    "SELECT COUNT(*), COUNT(DISTINCT host_key) FROM cookies WHERE lower(host_key)='discord.com' OR lower(host_key) LIKE '%.discord.com'"
                }
            }
        }
    };
    let (cookies, origins): (i64, i64) = connection
        .query_row(query, [], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(sqlite)?;
    Ok(SelectedCounts {
        cookies: u64::try_from(cookies).map_err(|_| SourceError::InvalidCookies)?,
        origins: u64::try_from(origins).map_err(|_| SourceError::InvalidCookies)?,
    })
}

/// Read every cookie for exactly the selected Slack or Discord domain family.
/// The source store is copied consistently; only private copies are queried by
/// `SQLite`. Encrypted cookies require an explicitly attached, profile-bound
/// adapter. Partitioned selected cookies fail explicitly.
///
/// # Errors
/// Refuses unavailable/changed sources, unsupported attributes, or bounded data
/// limits. No source permissions, database, WAL, or SHM contents are modified.
pub fn read_selected(
    grant: &SourceGrant,
    service: Service,
) -> Result<CapturedCookies, SourceError> {
    let (_temporary, connection, source_snapshot_digest) = snapshot(grant, service)?;
    let cookies = match grant.provider {
        Provider::Firefox => firefox(&connection, service)?,
        Provider::Chromium => chromium(&connection, service, grant.decryptor.as_deref())?,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| SourceError::Unavailable)?;
    let now = i64::try_from(now.as_secs()).map_err(|_| SourceError::Unavailable)?;
    let mut keys = BTreeSet::new();
    let mut bytes = 0usize;
    for cookie in &cookies {
        if cookie
            .expires_unix_seconds
            .is_some_and(|expiry| expiry <= now)
        {
            return Err(SourceError::InvalidCookies);
        }
        cookie
            .validate(service)
            .map_err(|_| SourceError::InvalidCookies)?;
        if !keys.insert((&cookie.domain, cookie.host_only, &cookie.name, &cookie.path)) {
            return Err(SourceError::InvalidCookies);
        }
        for value in [
            &cookie.host,
            &cookie.domain,
            &cookie.name,
            &cookie.value,
            &cookie.path,
        ] {
            bytes = bytes
                .checked_add(value.len())
                .ok_or(SourceError::LimitExceeded)?;
        }
    }
    if cookies.is_empty() {
        return Err(SourceError::Unavailable);
    }
    if bytes > MAX_SESSION_BYTES {
        return Err(SourceError::LimitExceeded);
    }
    Ok(CapturedCookies {
        cookies,
        source_snapshot_digest,
    })
}

fn selected_query(service: Service, firefox: bool) -> &'static str {
    match (service, firefox) {
        (Service::Slack, true) => {
            "SELECT host,name,value,path,expiry,isSecure,isHttpOnly,sameSite,originAttributes,isPartitionedAttributeSet,inBrowserElement FROM moz_cookies WHERE lower(host)='slack.com' OR lower(host) LIKE '%.slack.com' ORDER BY id"
        }
        (Service::Discord, true) => {
            "SELECT host,name,value,path,expiry,isSecure,isHttpOnly,sameSite,originAttributes,isPartitionedAttributeSet,inBrowserElement FROM moz_cookies WHERE lower(host)='discord.com' OR lower(host) LIKE '%.discord.com' ORDER BY id"
        }
        (Service::Slack, false) => {
            "SELECT host_key,name,value,path,expires_utc,is_secure,is_httponly,samesite,has_expires,encrypted_value,top_frame_site_key,source_scheme,source_port FROM cookies WHERE lower(host_key)='slack.com' OR lower(host_key) LIKE '%.slack.com' ORDER BY creation_utc"
        }
        (Service::Discord, false) => {
            "SELECT host_key,name,value,path,expires_utc,is_secure,is_httponly,samesite,has_expires,encrypted_value,top_frame_site_key,source_scheme,source_port FROM cookies WHERE lower(host_key)='discord.com' OR lower(host_key) LIKE '%.discord.com' ORDER BY creation_utc"
        }
    }
}
fn firefox(connection: &Connection, service: Service) -> Result<Vec<Cookie>, SourceError> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(sqlite)?;
    // Schema 15 removed rawSameSite, including the distinction between unset
    // and explicit None. Schema 16 changed persistent expiry to milliseconds.
    if !matches!(version, 15..=17) {
        return Err(SourceError::UnsupportedSchema);
    }
    let mut statement = connection
        .prepare(selected_query(service, true))
        .map_err(sqlite)?;
    let mut rows = statement.query([]).map_err(sqlite)?;
    let mut cookies = Vec::new();
    while let Some(row) = rows.next().map_err(sqlite)? {
        if cookies.len() == MAX_COOKIES {
            return Err(SourceError::LimitExceeded);
        }
        let attributes: String = row.get(8).map_err(sqlite)?;
        if !attributes.is_empty()
            || row.get::<_, i64>(9).map_err(sqlite)? != 0
            || row.get::<_, i64>(10).map_err(sqlite)? != 0
        {
            return Err(SourceError::UnsupportedPartition);
        }
        let raw_host: String = row.get(0).map_err(sqlite)?;
        let (domain, host_only) = host(&raw_host);
        let expiry: i64 = row.get(4).map_err(sqlite)?;
        let expiry = if version >= 16 {
            expiry.div_euclid(1000)
        } else {
            expiry
        };
        // Firefox does not persist session cookies in cookies.sqlite. A zero
        // expiry must never turn an expired persistent credential into a session.
        push_cookie(
            &mut cookies,
            Cookie {
                host: domain.clone(),
                domain,
                host_only,
                name: row.get(1).map_err(sqlite)?,
                value: row.get(2).map_err(sqlite)?,
                path: row.get(3).map_err(sqlite)?,
                expires_unix_seconds: Some(expiry),
                secure: boolean(row.get(5).map_err(sqlite)?)?,
                http_only: boolean(row.get(6).map_err(sqlite)?)?,
                same_site: same_site(row.get(7).map_err(sqlite)?, true)?,
            },
            service,
        )?;
    }
    Ok(cookies)
}
fn chromium(
    connection: &Connection,
    service: Service,
    decryptor: Option<&dyn ChromiumDecryptor>,
) -> Result<Vec<Cookie>, SourceError> {
    let version = connection
        .query_row("SELECT value FROM meta WHERE key='version'", [], |row| {
            Ok(match row.get_ref(0)? {
                rusqlite::types::ValueRef::Integer(version @ (23 | 24)) => Some(version),
                rusqlite::types::ValueRef::Text(b"23") => Some(23),
                rusqlite::types::ValueRef::Text(b"24") => Some(24),
                _ => None,
            })
        })
        .map_err(sqlite)?;
    let version = version.ok_or(SourceError::UnsupportedSchema)?;
    let mut statement = connection
        .prepare(selected_query(service, false))
        .map_err(sqlite)?;
    let mut rows = statement.query([]).map_err(sqlite)?;
    let mut cookies = Vec::new();
    while let Some(row) = rows.next().map_err(sqlite)? {
        if cookies.len() == MAX_COOKIES {
            return Err(SourceError::LimitExceeded);
        }
        let encrypted = Zeroizing::new(row.get::<_, Vec<u8>>(9).map_err(sqlite)?);
        if !encrypted.is_empty() && decryptor.is_none() {
            return Err(SourceError::UnsupportedEncryption);
        }
        let partition: String = row.get(10).map_err(sqlite)?;
        if !partition.is_empty() {
            return Err(SourceError::UnsupportedPartition);
        }
        let scheme: i64 = row.get(11).map_err(sqlite)?;
        let port: i64 = row.get(12).map_err(sqlite)?;
        if scheme != 2 || !matches!(port, -1 | 443) {
            return Err(SourceError::InvalidCookies);
        }
        let raw_host: String = row.get(0).map_err(sqlite)?;
        let plaintext = Zeroizing::new(row.get::<_, String>(2).map_err(sqlite)?);
        let value = if encrypted.is_empty() {
            plaintext
        } else {
            if !plaintext.is_empty() {
                return Err(SourceError::InvalidCookies);
            }
            decryptor
                .ok_or(SourceError::UnsupportedEncryption)?
                .decrypt(&raw_host, version, &encrypted)?
        };
        let (domain, host_only) = host(&raw_host);
        let expiry: i64 = row.get(4).map_err(sqlite)?;
        let expires = if boolean(row.get(8).map_err(sqlite)?)? {
            Some(
                expiry
                    .checked_sub(CHROMIUM_EPOCH_MICROS)
                    .ok_or(SourceError::InvalidCookies)?
                    .div_euclid(1_000_000),
            )
        } else {
            None
        };
        push_cookie(
            &mut cookies,
            Cookie {
                host: domain.clone(),
                domain,
                host_only,
                name: row.get(1).map_err(sqlite)?,
                value: value.to_string(),
                path: row.get(3).map_err(sqlite)?,
                expires_unix_seconds: expires,
                secure: boolean(row.get(5).map_err(sqlite)?)?,
                http_only: boolean(row.get(6).map_err(sqlite)?)?,
                same_site: same_site(row.get(7).map_err(sqlite)?, false)?,
            },
            service,
        )?;
    }
    Ok(cookies)
}

fn push_cookie(
    cookies: &mut Vec<Cookie>,
    cookie: Cookie,
    service: Service,
) -> Result<(), SourceError> {
    cookie
        .validate(service)
        .map_err(|_| SourceError::InvalidCookies)?;
    let mut bytes = 0usize;
    for entry in cookies.iter().chain(std::iter::once(&cookie)) {
        for value in [
            &entry.host,
            &entry.domain,
            &entry.name,
            &entry.value,
            &entry.path,
        ] {
            bytes = bytes
                .checked_add(value.len())
                .ok_or(SourceError::LimitExceeded)?;
        }
    }
    if bytes > MAX_SESSION_BYTES {
        return Err(SourceError::LimitExceeded);
    }
    cookies.push(cookie);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn firefox_fixture(version: i64) -> (tempfile::TempDir, Connection, SourceGrant) {
        let directory = tempfile::tempdir().expect("private source fixture");
        let connection =
            Connection::open(directory.path().join("cookies.sqlite")).expect("fixture DB");
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             CREATE TABLE moz_cookies(id INTEGER PRIMARY KEY,host TEXT,name TEXT,value TEXT,path TEXT,
                expiry INTEGER,isSecure INTEGER,isHttpOnly INTEGER,sameSite INTEGER,
                originAttributes TEXT,isPartitionedAttributeSet INTEGER,inBrowserElement INTEGER);",
        ).expect("fixture schema");
        connection
            .pragma_update(None, "user_version", version)
            .expect("source schema version");
        let grant = SourceGrant::new(
            File::open(directory.path()).expect("held profile"),
            Provider::Firefox,
        )
        .expect("explicit fixture grant");
        (directory, connection, grant)
    }
    fn firefox_row(connection: &Connection, name: &str, expiry: i64, same_site: i64) {
        connection
            .execute(
                "INSERT INTO moz_cookies(host,name,value,path,expiry,isSecure,isHttpOnly,sameSite,
                originAttributes,isPartitionedAttributeSet,inBrowserElement)
                VALUES('.slack.com',?1,'synthetic-cookie-secret','/',?2,1,1,?3,'',0,0)",
                rusqlite::params![name, expiry, same_site],
            )
            .expect("synthetic selected cookie");
    }
    fn unchanged_files(path: &Path, name: &str) -> Vec<(Vec<u8>, u32)> {
        [
            name.to_owned(),
            format!("{name}-wal"),
            format!("{name}-shm"),
        ]
        .iter()
        .map(|name| {
            let path = path.join(name);
            (
                std::fs::read(&path).expect("source bytes"),
                std::fs::metadata(path)
                    .expect("source metadata")
                    .permissions()
                    .mode(),
            )
        })
        .collect()
    }

    #[test]
    fn firefox_current_wal_expiry_unset_and_source_bytes_are_preserved() {
        let (directory, connection, grant) = firefox_fixture(17);
        firefox_row(&connection, "token", 2_000_000_123_000, 256);
        connection
            .execute_batch(
                "INSERT INTO moz_cookies(host,name,value) VALUES('unrelated.example','bad','');",
            )
            .expect("unrelated incomplete row");
        let before = unchanged_files(directory.path(), "cookies.sqlite");
        let captured = read_selected(&grant, Service::Slack).expect("selected WAL source");
        assert_eq!(captured.cookies.len(), 1);
        let cookie = &captured.cookies[0];
        assert_eq!(cookie.domain, "slack.com");
        assert!(!cookie.host_only);
        assert_eq!(cookie.expires_unix_seconds, Some(2_000_000_123));
        assert_eq!(cookie.same_site, SameSite::Unspecified);
        assert_eq!(unchanged_files(directory.path(), "cookies.sqlite"), before);
        assert!(!format!("{captured:?}").contains("synthetic-cookie-secret"));
        assert_eq!(
            read_selected(&grant, Service::Slack)
                .expect("same snapshot")
                .source_snapshot_digest,
            captured.source_snapshot_digest
        );
        assert_eq!(
            read_selected(&grant, Service::Discord).expect_err("no Discord session"),
            SourceError::Unavailable
        );
    }

    #[test]
    fn selected_service_counts_are_value_free_and_source_preserving() {
        let (directory, connection, grant) = firefox_fixture(17);
        firefox_row(&connection, "token", 2_000_000_123_000, 256);
        connection
            .execute_batch(
                "INSERT INTO moz_cookies(host,name,value,path,expiry,isSecure,isHttpOnly,sameSite,
                 originAttributes,isPartitionedAttributeSet,inBrowserElement)
                 VALUES('app.slack.com','session','synthetic-cookie-secret','/',2000000000000,1,1,256,'',0,0);
                 INSERT INTO moz_cookies(host,name,value,path,expiry,isSecure,isHttpOnly,sameSite,
                 originAttributes,isPartitionedAttributeSet,inBrowserElement)
                 VALUES('.discord.com','token','synthetic-discord-secret','/',2000000000000,1,1,256,'',0,0);
                 INSERT INTO moz_cookies(host,name,value,path,expiry,isSecure,isHttpOnly,sameSite,
                 originAttributes,isPartitionedAttributeSet,inBrowserElement)
                 VALUES('.example.com','cookie','synthetic-other-secret','/',2000000000000,1,1,256,'',0,0);",
            )
            .expect("synthetic service rows");
        let before = unchanged_files(directory.path(), "cookies.sqlite");
        let slack = count_selected(&grant, Service::Slack).expect("value-free Slack count");
        assert_eq!(slack.cookies, 2);
        assert_eq!(slack.origins, 2);
        let discord = count_selected(&grant, Service::Discord).expect("value-free Discord count");
        assert_eq!(discord.cookies, 1);
        assert_eq!(discord.origins, 1);
        assert_eq!(unchanged_files(directory.path(), "cookies.sqlite"), before);
        assert!(!format!("{slack:?}").contains("synthetic-cookie-secret"));
        assert!(!format!("{discord:?}").contains("synthetic-discord-secret"));
    }

    #[test]
    fn firefox_version_and_persistent_expiry_are_not_guessed() {
        let (_directory, connection, grant) = firefox_fixture(15);
        firefox_row(&connection, "token", 2_000_000_123, 1);
        assert_eq!(
            read_selected(&grant, Service::Slack)
                .expect("seconds schema")
                .cookies[0]
                .expires_unix_seconds,
            Some(2_000_000_123)
        );
        connection
            .execute_batch("UPDATE moz_cookies SET expiry=0;")
            .expect("expired persistent");
        assert_eq!(
            read_selected(&grant, Service::Slack).expect_err("not session cookie"),
            SourceError::InvalidCookies
        );
        connection
            .pragma_update(None, "user_version", 18)
            .expect("unknown schema");
        assert_eq!(
            read_selected(&grant, Service::Slack).expect_err("future schema"),
            SourceError::UnsupportedSchema
        );
    }

    #[test]
    fn selected_firefox_partitioning_and_overflow_fail_without_truncation() {
        let (_directory, connection, grant) = firefox_fixture(16);
        firefox_row(&connection, "token", 2_000_000_123_000, 1);
        connection
            .execute_batch("UPDATE moz_cookies SET isPartitionedAttributeSet=1;")
            .expect("partitioned source");
        assert_eq!(
            read_selected(&grant, Service::Slack).expect_err("partition not silently dropped"),
            SourceError::UnsupportedPartition
        );
        connection
            .execute_batch("UPDATE moz_cookies SET isPartitionedAttributeSet=0;")
            .expect("unpartitioned source");
        for ordinal in 1..=MAX_COOKIES {
            firefox_row(
                &connection,
                &format!("token{ordinal}"),
                2_000_000_123_000,
                1,
            );
        }
        assert_eq!(
            read_selected(&grant, Service::Slack).expect_err("all rows or refusal"),
            SourceError::LimitExceeded
        );
    }

    fn chromium_fixture() -> (tempfile::TempDir, Connection, SourceGrant) {
        let directory = tempfile::tempdir().expect("private source fixture");
        std::fs::create_dir(directory.path().join("Network")).expect("profile network");
        let connection =
            Connection::open(directory.path().join("Network/Cookies")).expect("fixture DB");
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             CREATE TABLE meta(key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY,value LONGVARCHAR);
             INSERT INTO meta VALUES('version','24');
             CREATE TABLE cookies(creation_utc INTEGER,host_key TEXT,name TEXT,value TEXT,path TEXT,
                 expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,
                 has_expires INTEGER,encrypted_value BLOB,top_frame_site_key TEXT,source_scheme INTEGER,source_port INTEGER);
             INSERT INTO cookies VALUES(1,'discord.com','token','synthetic-session','/',0,1,1,-1,0,X'','',2,443);
             INSERT INTO cookies VALUES(2,'.discord.com','persist','synthetic-persistent','/',13644473600000000,1,1,1,1,X'','',2,-1);
             INSERT INTO cookies VALUES(3,'evil-discord.com','ignored','',NULL,0,0,0,-1,0,X'01','',0,80);",
        ).expect("synthetic Chromium schema and rows");
        let grant = SourceGrant::new(
            File::open(directory.path()).expect("held profile"),
            Provider::Chromium,
        )
        .expect("explicit fixture grant");
        (directory, connection, grant)
    }

    #[test]
    fn chromium_session_persistent_encryption_and_partition_selection() {
        let (directory, connection, grant) = chromium_fixture();
        let before = unchanged_files(&directory.path().join("Network"), "Cookies");
        let captured = read_selected(&grant, Service::Discord).expect("selected plaintext cookies");
        assert_eq!(captured.cookies.len(), 2);
        assert!(captured.cookies[0].host_only);
        assert_eq!(captured.cookies[0].expires_unix_seconds, None);
        assert_eq!(captured.cookies[0].same_site, SameSite::Unspecified);
        assert!(!captured.cookies[1].host_only);
        assert_eq!(
            captured.cookies[1].expires_unix_seconds,
            Some(2_000_000_000)
        );
        assert_eq!(
            unchanged_files(&directory.path().join("Network"), "Cookies"),
            before
        );
        connection
            .execute_batch("UPDATE cookies SET encrypted_value=X'0102' WHERE name='token';")
            .expect("encrypted selected credential");
        assert_eq!(
            read_selected(&grant, Service::Discord).expect_err("encrypted adapter absent"),
            SourceError::UnsupportedEncryption
        );
        connection.execute_batch("UPDATE cookies SET encrypted_value=X'',top_frame_site_key='https://other.example' WHERE name='token';").expect("partitioned selected credential");
        assert_eq!(
            read_selected(&grant, Service::Discord).expect_err("partition unsupported"),
            SourceError::UnsupportedPartition
        );
    }

    #[test]
    fn chromium_schema_and_expired_persistent_cookies_refuse_explicitly() {
        let (directory, connection, grant) = chromium_fixture();
        connection
            .execute_batch("UPDATE meta SET value=23 WHERE key='version';")
            .expect("recognized prior schema");
        assert_eq!(
            read_selected(&grant, Service::Discord)
                .expect("schema 23")
                .cookies
                .len(),
            2
        );
        connection
            .execute_batch("UPDATE meta SET value=25 WHERE key='version';")
            .expect("unsupported future schema");
        assert_eq!(
            read_selected(&grant, Service::Discord).expect_err("no schema guessing"),
            SourceError::UnsupportedSchema
        );
        connection.execute_batch("UPDATE meta SET value=24 WHERE key='version'; UPDATE cookies SET expires_utc=11644473601000000 WHERE name='persist';")
            .expect("expired selected persistent cookie");
        let before = unchanged_files(&directory.path().join("Network"), "Cookies");
        assert_eq!(
            read_selected(&grant, Service::Discord)
                .expect_err("expired cookie cannot become a session"),
            SourceError::InvalidCookies
        );
        assert_eq!(
            unchanged_files(&directory.path().join("Network"), "Cookies"),
            before
        );
    }

    #[test]
    fn chromium_symlinked_network_never_falls_back_to_root_cookie_store() {
        let directory = tempfile::tempdir().expect("private source fixture");
        let elsewhere = tempfile::tempdir().expect("ungranted fixture");
        symlink(elsewhere.path(), directory.path().join("Network")).expect("malicious source link");
        std::fs::write(directory.path().join("Cookies"), b"not a fallback grant")
            .expect("root candidate");
        let grant = SourceGrant::new(
            File::open(directory.path()).expect("held profile"),
            Provider::Chromium,
        )
        .expect("explicit fixture grant");
        assert_eq!(
            read_selected(&grant, Service::Discord).expect_err("nofollow must fail"),
            SourceError::Unavailable
        );
    }
}

// The native fixture observes the canonical held-source snapshot. It never
// constructs a cookie database or supplies encrypted rows to the reader.
#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
pub(super) fn verify_official_fixture_store(grant: &SourceGrant) -> Result<bool, SourceError> {
    let (_stage, database, _digest) = snapshot(grant, Service::Slack)?;
    let version: i64 = database
        .query_row("SELECT value FROM meta WHERE key='version'", [], |row| {
            Ok(match row.get_ref(0)? {
                rusqlite::types::ValueRef::Integer(24) | rusqlite::types::ValueRef::Text(b"24") => {
                    24
                }
                _ => 0,
            })
        })
        .map_err(|_| SourceError::Unavailable)?;
    let count: i64 = database
        .query_row("SELECT count(*) FROM cookies", [], |row| row.get(0))
        .map_err(|_| SourceError::Unavailable)?;
    if version != 24 || count != 1 {
        return Ok(false);
    }
    let (plain, encrypted): (String, Vec<u8>) = database.query_row(
        "SELECT value, encrypted_value FROM cookies WHERE host_key='app.slack.com' AND name='__Host-tilo-source-fixture'", [],
        |row| Ok((row.get(0)?, row.get(1)?))
    ).map_err(|_| SourceError::Unavailable)?;
    let plain = Zeroizing::new(plain);
    let encrypted = Zeroizing::new(encrypted);
    Ok(plain.is_empty() && encrypted.starts_with(b"v11") && encrypted.len() > 19)
}
