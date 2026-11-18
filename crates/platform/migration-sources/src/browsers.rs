//! Where a home keeps its browsers, and the passwords each browser saved.
//!
//! [`LOCATIONS`] is the one list of browser installations the installer's account scan and
//! `tilo migrate review browsers` both read: under the config directory, under the home, and
//! inside a Flatpak's own directory. [`profiles`] finds the profiles of an installation (a
//! Chromium-family `Default`/`Profile N`, the sections of Firefox's `profiles.ini`);
//! bookmarks and history are read by `browser-continuity`, passwords here ([`saved_logins`]).
//!
//! A password read here is held only in wiped memory and handed straight to the vault it is
//! moving to; it is never written, printed or put in a resource.

use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::thunderbird::{LoginsError, SavedLogin};

/// The engine a browser is built on, which decides how its profiles are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Chromium,
    Firefox,
}

/// Where an installation's directory is: under the config directory, the home, or a Flatpak's
/// own directory under the home.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Root {
    Config,
    Home,
    /// `~/.var/app/<id>/<path>`.
    Flatpak(&'static str),
}

/// One browser installation a home may have.
#[derive(Clone, Copy, Debug)]
pub struct Location {
    pub root: Root,
    /// Its directory under the root.
    pub path: &'static str,
    pub engine: Engine,
    /// The browser's name.
    pub label: &'static str,
}

const fn at(root: Root, path: &'static str, engine: Engine, label: &'static str) -> Location {
    Location {
        root,
        path,
        engine,
        label,
    }
}

/// Every browser installation a home may have.
pub const LOCATIONS: &[Location] = &[
    at(Root::Config, "chromium", Engine::Chromium, "Chromium"),
    at(
        Root::Config,
        "google-chrome",
        Engine::Chromium,
        "Google Chrome",
    ),
    at(
        Root::Config,
        "ungoogled-chromium",
        Engine::Chromium,
        "Ungoogled Chromium",
    ),
    at(
        Root::Config,
        "BraveSoftware/Brave-Browser",
        Engine::Chromium,
        "Brave",
    ),
    at(Root::Home, ".mozilla/firefox", Engine::Firefox, "Firefox"),
    at(Root::Home, ".librewolf", Engine::Firefox, "LibreWolf"),
    at(
        Root::Flatpak("org.mozilla.firefox"),
        ".mozilla/firefox",
        Engine::Firefox,
        "Firefox",
    ),
    at(
        Root::Flatpak("org.chromium.Chromium"),
        "config/chromium",
        Engine::Chromium,
        "Chromium",
    ),
    at(
        Root::Flatpak("com.google.Chrome"),
        "config/google-chrome",
        Engine::Chromium,
        "Google Chrome",
    ),
    at(
        Root::Flatpak("io.github.ungoogled_software.ungoogled_chromium"),
        "config/ungoogled-chromium",
        Engine::Chromium,
        "Ungoogled Chromium",
    ),
    at(
        Root::Flatpak("io.gitlab.librewolf-community"),
        ".librewolf",
        Engine::Firefox,
        "LibreWolf",
    ),
];

impl Location {
    /// Its directory, in `home` whose config directory is `config`.
    #[must_use]
    pub fn directory(&self, home: &Path, config: &Path) -> PathBuf {
        match self.root {
            Root::Config => config.join(self.path),
            Root::Home => home.join(self.path),
            Root::Flatpak(app) => home.join(".var/app").join(app).join(self.path),
        }
    }
}

/// One browser profile.
#[derive(Clone, Debug)]
pub struct BrowserProfile {
    /// `Firefox · default-release`.
    pub label: String,
    pub engine: Engine,
    /// The browser's name.
    pub browser: &'static str,
    pub directory: PathBuf,
}

/// Every profile of every browser installed in `home`, whose config directory is `config`.
#[must_use]
pub fn profiles(home: &Path, config: &Path) -> Vec<BrowserProfile> {
    let mut found = Vec::new();
    for location in LOCATIONS {
        let directory = location.directory(home, config);
        if !std::fs::symlink_metadata(&directory).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        match location.engine {
            Engine::Chromium => chromium_profiles(location, &directory, &mut found),
            Engine::Firefox => firefox_profiles(location, &directory, &mut found),
        }
    }
    found
}

fn chromium_profiles(location: &Location, directory: &Path, found: &mut Vec<BrowserProfile>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name == "Default" || name.starts_with("Profile "))
        .collect();
    names.sort();
    for name in names {
        let profile = directory.join(&name);
        if ["Bookmarks", "History", "Login Data"]
            .iter()
            .any(|file| profile.join(file).is_file())
        {
            found.push(BrowserProfile {
                label: format!("{} · {name}", location.label),
                engine: Engine::Chromium,
                browser: location.label,
                directory: profile,
            });
        }
    }
}

fn firefox_profiles(location: &Location, directory: &Path, found: &mut Vec<BrowserProfile>) {
    let Ok(ini) = std::fs::read_to_string(directory.join("profiles.ini")) else {
        return;
    };
    for (name, path, relative) in firefox_sections(&ini) {
        let name = if name.is_empty() { path.clone() } else { name };
        let profile = if relative == Some(true) {
            directory.join(&path)
        } else {
            PathBuf::from(&path)
        };
        if profile.join("places.sqlite").is_file() || profile.join("logins.json").is_file() {
            found.push(BrowserProfile {
                label: format!("{} · {name}", location.label),
                engine: Engine::Firefox,
                browser: location.label,
                directory: profile,
            });
        }
    }
}

/// Each `[Profile…]` section of a Firefox `profiles.ini`: its name (empty when it has none),
/// its path, and whether the path is relative to the ini's directory (`IsRelative=1`; `None`
/// when the section does not say). A section without a path is left out.
#[must_use]
pub fn firefox_sections(ini: &str) -> Vec<(String, String, Option<bool>)> {
    let mut sections = Vec::new();
    let mut current: Option<(String, Option<String>, Option<bool>)> = None;
    let mut flush = |current: &mut Option<(String, Option<String>, Option<bool>)>| {
        if let Some((name, Some(path), relative)) = current.take() {
            sections.push((name, path, relative));
        }
    };
    for line in ini.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') {
            flush(&mut current);
            current = line
                .starts_with("[Profile")
                .then(|| (String::new(), None, None));
        } else if let Some((key, value)) = line.split_once('=')
            && let Some((name, path, relative)) = current.as_mut()
        {
            let value = value.trim();
            match key.trim() {
                "Name" => value.clone_into(name),
                "Path" => *path = Some(value.to_owned()),
                "IsRelative" => *relative = Some(value == "1"),
                _ => {}
            }
        }
    }
    flush(&mut current);
    sections
}

/// The largest password database read.
const MAX_LOGIN_DATABASE_BYTES: u64 = 64 * 1024 * 1024;
/// The most saved logins one profile is read for.
const MAX_LOGINS: usize = 10_000;

/// The passwords `profile` saved, decrypted: Firefox's through NSS (as Thunderbird's), a
/// Chromium-family browser's from `Login Data` with its `OSCrypt` key.
///
/// # Errors
/// See [`LoginsError`]: none saved, a primary password the person has to type, or unreadable.
pub fn saved_logins(profile: &BrowserProfile) -> Result<Vec<SavedLogin>, LoginsError> {
    match profile.engine {
        Engine::Firefox => {
            let read = |name: &str| {
                bounded_read(
                    &profile.directory.join(name),
                    crate::thunderbird::logins::MAX_NSS_FILE_BYTES,
                )
            };
            let logins = match read("logins.json") {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(LoginsError::NoneSaved);
                }
                Err(error) => return Err(LoginsError::Unreadable(error.to_string())),
            };
            let key4 = read("key4.db")
                .map_err(|error| LoginsError::Unreadable(format!("key4.db: {error}")))?;
            crate::thunderbird::logins::nss_logins(&logins, &key4)
        }
        Engine::Chromium => chromium_logins(profile),
    }
}

fn bounded_read(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other(
            "larger than a browser's file is read",
        ));
    }
    Ok(bytes)
}

/// A Chromium-family profile's `Login Data`, each password decrypted with its `OSCrypt` key:
/// `v10` with Chromium's fixed one, `v11` with the one the system keyring keeps for the brand.
fn chromium_logins(profile: &BrowserProfile) -> Result<Vec<SavedLogin>, LoginsError> {
    use std::io::Write as _;
    let unreadable = |what: &str| LoginsError::Unreadable(format!("Login Data: {what}"));
    let bytes = match bounded_read(
        &profile.directory.join("Login Data"),
        MAX_LOGIN_DATABASE_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(LoginsError::NoneSaved);
        }
        Err(error) => return Err(unreadable(&error.to_string())),
    };
    // The browser may hold the database open; a copy reads as it was.
    let mut copy = tempfile::Builder::new()
        .prefix("tilo-browser-logins-")
        .tempfile()
        .map_err(|error| unreadable(&error.to_string()))?;
    copy.write_all(&bytes)
        .and_then(|()| copy.flush())
        .map_err(|error| unreadable(&error.to_string()))?;
    let connection = rusqlite::Connection::open_with_flags(
        copy.path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| unreadable("not a database"))?;
    let mut statement = connection
        .prepare("SELECT origin_url, username_value, password_value FROM logins WHERE blacklisted_by_user = 0")
        .map_err(|_| unreadable("no logins table"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(|_| unreadable("no logins table"))?;
    let v10 = tilo_seal::compat::chromium_oscrypt_key(b"peanuts");
    let mut v11: Option<Option<Zeroizing<[u8; 16]>>> = None;
    let mut found = Vec::new();
    let mut locked = false;
    for (origin, username, sealed) in rows.filter_map(Result::ok).take(MAX_LOGINS) {
        let Some((prefix, ciphertext)) = sealed.split_at_checked(3) else {
            continue;
        };
        let key = match prefix {
            b"v10" => v10.clone(),
            b"v11" => {
                let key = v11.get_or_insert_with(|| keyring_key(profile));
                let Some(key) = key.clone() else {
                    locked = true;
                    continue;
                };
                key
            }
            _ => continue,
        };
        let Ok(plaintext) = tilo_seal::compat::chromium_oscrypt_plaintext(&key, ciphertext) else {
            continue;
        };
        let Ok(password) = String::from_utf8(plaintext.to_vec()) else {
            continue;
        };
        found.push(SavedLogin {
            origin,
            username,
            password: Zeroizing::new(password),
        });
    }
    if found.is_empty() {
        return Err(if locked {
            LoginsError::Locked
        } else {
            LoginsError::NoneSaved
        });
    }
    Ok(found)
}

/// The `v11` key the system keyring keeps for `profile`'s browser, when it keeps one and the
/// browser is one whose keyring entry is known.
#[cfg(target_os = "linux")]
fn keyring_key(profile: &BrowserProfile) -> Option<Zeroizing<[u8; 16]>> {
    use tilo_secrets::owner_migration::{ChromiumBrand, ChromiumKey};
    let brand = match profile.browser {
        "Google Chrome" => ChromiumBrand::Chrome,
        "Chromium" | "Ungoogled Chromium" => ChromiumBrand::Chromium,
        _ => return None,
    };
    let directory = std::fs::File::open(&profile.directory).ok()?;
    let password = ChromiumKey::new(&directory, brand)
        .ok()?
        .read_blocking()
        .ok()??;
    Some(tilo_seal::compat::chromium_oscrypt_key(password.expose()))
}

#[cfg(not(target_os = "linux"))]
fn keyring_key(_: &BrowserProfile) -> Option<Zeroizing<[u8; 16]>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_firefox_index_names_each_profile_and_where_it_is() {
        let ini = "[Install4F96D1932A9F858E]\nDefault=x.default-release\n\n\
                   [Profile1]\nName=default\nIsRelative=1\nPath=y.default\n\n\
                   [Profile0]\nName=default-release\nIsRelative=0\nPath=/data/ff\n\n[General]\nVersion=2\n";
        assert_eq!(
            firefox_sections(ini),
            [
                ("default".to_owned(), "y.default".to_owned(), Some(true)),
                (
                    "default-release".to_owned(),
                    "/data/ff".to_owned(),
                    Some(false)
                ),
            ]
        );
    }

    #[test]
    fn a_chromium_profiles_v10_passwords_open_with_chromiums_fixed_key() {
        let home = tempfile::tempdir().unwrap();
        let profile = home.path().join(".config/chromium/Default");
        std::fs::create_dir_all(&profile).unwrap();
        let connection = rusqlite::Connection::open(profile.join("Login Data")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE logins (origin_url TEXT, username_value TEXT, \
                 password_value BLOB, blacklisted_by_user INTEGER);",
            )
            .unwrap();
        let key = tilo_seal::compat::chromium_oscrypt_key(b"peanuts");
        let mut sealed = b"v10".to_vec();
        sealed.extend(tilo_seal::compat::chromium_oscrypt_ciphertext(&key, b"hunter2").unwrap());
        connection
            .execute(
                "INSERT INTO logins VALUES ('https://example.com/', 'ada', ?1, 0)",
                [sealed],
            )
            .unwrap();
        drop(connection);
        let found = profiles(home.path(), &home.path().join(".config"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "Chromium · Default");
        let logins = saved_logins(&found[0]).unwrap();
        assert_eq!(logins.len(), 1);
        assert_eq!(logins[0].username, "ada");
        assert_eq!(logins[0].password.as_str(), "hunter2");
    }
}
