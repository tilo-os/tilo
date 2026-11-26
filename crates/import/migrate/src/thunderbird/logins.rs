//! The passwords a Thunderbird profile saved for its mail servers.
//!
//! Thunderbird keeps them as Firefox does, through NSS: `logins.json` holds
//! each saved login's origin (`imap://imap.example.com`) beside its user
//! name and password, each encrypted under a key that `key4.db` keeps
//! encrypted in turn under the profile's primary password (empty unless the
//! person set one). The cryptography is `tilo-seal`'s
//! ([`tilo_seal::compat`]); this module reads the files and their DER.
//!
//! A password read here is held only in wiped memory and handed straight to
//! the vault it is moving to; it is never written, printed or put in a
//! record.

use std::path::Path;

use base64::Engine as _;
use tilo_seal::compat::{NssCipher, nss_cbc_plaintext, nss_pbes2_plaintext};
use zeroize::Zeroizing;

use super::{find_reviewed, read_file, scan};

/// The largest `key4.db` or `logins.json` read.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// The most saved logins one profile is read for.
const MAX_LOGINS: usize = 10_000;
/// What `key4.db`'s password check decrypts to when the primary password is
/// right.
const PASSWORD_CHECK: &[u8] = b"password-check";
/// The id NSS keeps the logins key under in `nssPrivate.a102`.
pub(crate) const LOGINS_KEY_ID: [u8; 16] = [0xf8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

const OID_PBES2: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x05, 0x0d];
const OID_AES256_CBC: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x01, 0x2a];
const OID_DES_EDE3_CBC: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x03, 0x07];

/// One saved login: the server it is for, the user name and the password.
pub struct SavedLogin {
    /// Where Thunderbird uses it: `imap://imap.example.com`,
    /// `smtp://smtp.example.com`.
    pub origin: String,
    pub username: String,
    pub password: Zeroizing<String>,
}

impl std::fmt::Debug for SavedLogin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SavedLogin")
            .field("origin", &self.origin)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// Why a profile's saved passwords could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginsError {
    /// The profile saved no passwords.
    NoneSaved,
    /// A primary password protects them: only the person can open them.
    Locked,
    /// They are kept in a form this reader does not read, or damaged.
    Unreadable(String),
}

impl std::fmt::Display for LoginsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoneSaved => formatter.write_str("Thunderbird saved no passwords"),
            Self::Locked => formatter
                .write_str("Thunderbird's saved passwords are locked with a primary password"),
            Self::Unreadable(why) => write!(
                formatter,
                "Thunderbird's saved passwords could not be read: {why}"
            ),
        }
    }
}

impl std::error::Error for LoginsError {}

/// The logins the Thunderbird profile `profile` (its `source_id`) under
/// `home` saved, opened with no primary password.
///
/// # Errors
/// See [`LoginsError`].
pub fn saved_logins(home: &Path, profile: &str) -> Result<Vec<SavedLogin>, LoginsError> {
    let scan = scan(home);
    let reviewed = find_reviewed(&scan.reviewed, profile)
        .ok_or_else(|| LoginsError::Unreadable(format!("no Thunderbird profile {profile:?}")))?;
    let logins = match read_file(&reviewed.root, "logins.json", MAX_FILE_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(LoginsError::NoneSaved);
        }
        Err(error) => return Err(LoginsError::Unreadable(error.to_string())),
    };
    let key4 = read_file(&reviewed.root, "key4.db", MAX_FILE_BYTES)
        .map_err(|error| LoginsError::Unreadable(format!("key4.db: {error}")))?;
    nss_logins(&logins, &key4)
}

/// The logins an NSS profile (Thunderbird's, Firefox's) keeps: its `logins.json` and `key4.db`,
/// opened with no primary password.
///
/// # Errors
/// See [`LoginsError`].
pub fn nss_logins(logins: &[u8], key4: &[u8]) -> Result<Vec<SavedLogin>, LoginsError> {
    let key = logins_key(key4, b"")?;
    decrypt_logins(logins, &key)
}

/// The largest `logins.json` or `key4.db` read.
pub const MAX_NSS_FILE_BYTES: u64 = MAX_FILE_BYTES;

/// The saved password for `username` at a server of `scheme` (`imap`,
/// `smtp`, `pop3`) on `host`.
#[must_use]
pub fn password_for<'a>(
    logins: &'a [SavedLogin],
    scheme: &str,
    host: &str,
    username: &str,
) -> Option<&'a Zeroizing<String>> {
    let origin = format!("{scheme}://{host}").to_ascii_lowercase();
    logins
        .iter()
        .find(|login| {
            login
                .origin
                .trim_end_matches('/')
                .eq_ignore_ascii_case(&origin)
                && login.username.eq_ignore_ascii_case(username)
        })
        .map(|login| &login.password)
}

/// The key `key4.db` keeps for the logins, opened with `primary`.
fn logins_key(key4: &[u8], primary: &[u8]) -> Result<Zeroizing<Vec<u8>>, LoginsError> {
    use std::io::Write as _;
    let unreadable = |what: &str| LoginsError::Unreadable(format!("key4.db: {what}"));
    let mut temporary = tempfile::Builder::new()
        .prefix("tilo-thunderbird-keys-")
        .tempfile()
        .map_err(|error| unreadable(&error.to_string()))?;
    temporary
        .write_all(key4)
        .and_then(|()| temporary.flush())
        .map_err(|error| unreadable(&error.to_string()))?;
    let connection = rusqlite::Connection::open_with_flags(
        temporary.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| unreadable("not a database"))?;
    let (global_salt, check): (Vec<u8>, Vec<u8>) = connection
        .query_row(
            "SELECT item1, item2 FROM metaData WHERE id = 'password'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| unreadable("no password check"))?;
    let checked = pbes2(&global_salt, primary, &check)?;
    if !checked.starts_with(PASSWORD_CHECK) {
        return Err(LoginsError::Locked);
    }
    let mut statement = connection
        .prepare("SELECT a11, a102 FROM nssPrivate")
        .map_err(|_| unreadable("no private keys"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(|_| unreadable("no private keys"))?;
    for (sealed, id) in rows.filter_map(Result::ok) {
        if id == LOGINS_KEY_ID {
            return pbes2(&global_salt, primary, &sealed);
        }
    }
    Err(unreadable("no logins key"))
}

/// Decrypt one PBES2 entry of `key4.db`. A wrong primary password shows as
/// padding that does not check: [`LoginsError::Locked`].
fn pbes2(
    global_salt: &[u8],
    primary: &[u8],
    der: &[u8],
) -> Result<Zeroizing<Vec<u8>>, LoginsError> {
    let unreadable = |what: &str| LoginsError::Unreadable(format!("key4.db: {what}"));
    let mut outer = Der::sequence(der).ok_or_else(|| unreadable("an entry is not DER"))?;
    let mut algorithm = outer
        .sequence_in()
        .ok_or_else(|| unreadable("an entry names no algorithm"))?;
    if algorithm.oid().as_deref() != Some(OID_PBES2) {
        return Err(unreadable("an older key format"));
    }
    let mut parameters = algorithm.sequence_in().ok_or_else(|| unreadable("PBES2"))?;
    let mut kdf = parameters
        .sequence_in()
        .ok_or_else(|| unreadable("PBKDF2"))?;
    let _pbkdf2 = kdf.oid();
    let mut kdf_parameters = kdf.sequence_in().ok_or_else(|| unreadable("PBKDF2"))?;
    let entry_salt = kdf_parameters
        .octets()
        .ok_or_else(|| unreadable("PBKDF2 salt"))?;
    let iterations = kdf_parameters
        .integer()
        .and_then(|iterations| u32::try_from(iterations).ok())
        .ok_or_else(|| unreadable("PBKDF2 iterations"))?;
    let mut cipher = parameters
        .sequence_in()
        .ok_or_else(|| unreadable("cipher"))?;
    if cipher.oid().as_deref() != Some(OID_AES256_CBC) {
        return Err(unreadable("a cipher other than AES-256-CBC"));
    }
    let iv = cipher.octets().ok_or_else(|| unreadable("IV"))?;
    // NSS keeps 14 bytes of the IV; the DER header of a 14-byte string starts it.
    let iv = if iv.len() == 14 {
        [&[0x04, 0x0e][..], &iv].concat()
    } else {
        iv
    };
    let ciphertext = outer.octets().ok_or_else(|| unreadable("ciphertext"))?;
    nss_pbes2_plaintext(
        global_salt,
        primary,
        &entry_salt,
        iterations,
        &iv,
        &ciphertext,
    )
    .map_err(|_| LoginsError::Locked)
}

/// Every login of `logins.json`, decrypted with `key`.
fn decrypt_logins(logins: &[u8], key: &[u8]) -> Result<Vec<SavedLogin>, LoginsError> {
    let unreadable = |what: &str| LoginsError::Unreadable(format!("logins.json: {what}"));
    let document: serde_json::Value =
        serde_json::from_slice(logins).map_err(|_| unreadable("not JSON"))?;
    let entries = document
        .get("logins")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| unreadable("no logins"))?;
    let mut found = Vec::new();
    for entry in entries.iter().take(MAX_LOGINS) {
        let text = |name: &str| {
            entry
                .get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        };
        let (Some(username), Some(password)) = (
            decrypt_field(text("encryptedUsername"), key),
            decrypt_field(text("encryptedPassword"), key),
        ) else {
            continue;
        };
        let (Ok(username), Ok(password)) = (
            String::from_utf8(username.to_vec()),
            String::from_utf8(password.to_vec()),
        ) else {
            continue;
        };
        found.push(SavedLogin {
            origin: text("hostname").to_owned(),
            username,
            password: Zeroizing::new(password),
        });
    }
    if found.is_empty() {
        return Err(LoginsError::NoneSaved);
    }
    Ok(found)
}

/// One encrypted field of a saved login: base64 of DER naming the key, the
/// cipher and its IV, then the ciphertext.
fn decrypt_field(encoded: &str, key: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let der = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let mut outer = Der::sequence(&der)?;
    let _key_id = outer.octets()?;
    let mut algorithm = outer.sequence_in()?;
    let cipher = match algorithm.oid()?.as_slice() {
        OID_DES_EDE3_CBC => NssCipher::TripleDesCbc,
        OID_AES256_CBC => NssCipher::Aes256Cbc,
        _ => return None,
    };
    let iv = algorithm.octets()?;
    let ciphertext = outer.octets()?;
    nss_cbc_plaintext(cipher, key, &iv, &ciphertext).ok()
}

/// A reader over the elements of one DER sequence.
struct Der<'a> {
    rest: &'a [u8],
}

impl<'a> Der<'a> {
    /// The elements of the sequence `bytes` is.
    fn sequence(bytes: &'a [u8]) -> Option<Self> {
        let (tag, content, _) = element(bytes)?;
        (tag == 0x30).then_some(Self { rest: content })
    }

    fn next(&mut self, wanted: u8) -> Option<&'a [u8]> {
        let (tag, content, rest) = element(self.rest)?;
        if tag != wanted {
            return None;
        }
        self.rest = rest;
        Some(content)
    }

    fn sequence_in(&mut self) -> Option<Self> {
        self.next(0x30).map(|content| Self { rest: content })
    }

    fn oid(&mut self) -> Option<Vec<u8>> {
        self.next(0x06).map(<[u8]>::to_vec)
    }

    fn octets(&mut self) -> Option<Vec<u8>> {
        self.next(0x04).map(<[u8]>::to_vec)
    }

    fn integer(&mut self) -> Option<u64> {
        let content = self.next(0x02)?;
        if content.len() > 8 {
            return None;
        }
        Some(
            content
                .iter()
                .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte)),
        )
    }
}

/// The first DER element of `bytes`: its tag, its content and what follows it.
fn element(bytes: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = bytes.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 {
            return None;
        }
        let (digits, rest) = rest.split_at_checked(count)?;
        let length = digits
            .iter()
            .fold(0_usize, |value, byte| (value << 8) | usize::from(*byte));
        (length, rest)
    };
    let (content, rest) = rest.split_at_checked(length)?;
    Some((tag, content, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_profiles_saved_passwords_are_read_and_matched_to_their_servers() {
        let dir = tempfile::tempdir().unwrap();
        super::super::fixture::write_sample_profile(dir.path());
        let profile = super::super::discover(dir.path()).unwrap().remove(0);
        let logins = saved_logins(dir.path(), &profile.source_id).unwrap();
        assert_eq!(
            password_for(&logins, "imap", "imap.example.com", "ada@example.com")
                .map(|password| password.as_str()),
            Some(super::super::fixture::WORK_PASSWORD)
        );
        assert!(password_for(&logins, "imap", "imap.example.com", "someone@else").is_none());
        assert!(!format!("{logins:?}").contains(super::super::fixture::WORK_PASSWORD));
    }

    #[test]
    fn a_primary_password_locks_them() {
        let dir = tempfile::tempdir().unwrap();
        super::super::fixture::write_sample_profile(dir.path());
        let profile = super::super::discover(dir.path()).unwrap().remove(0);
        let key4 = std::fs::read(
            dir.path()
                .join(".thunderbird/fx8r2l9p.default-release/key4.db"),
        )
        .unwrap();
        assert!(logins_key(&key4, b"").is_ok());
        assert_eq!(
            logins_key(&key4, b"not the primary password").err(),
            Some(LoginsError::Locked)
        );
        drop(profile);
    }
}
