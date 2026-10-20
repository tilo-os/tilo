//! Exact legacy browser profiles discovered inside an already granted home.
//! Metadata discovery never reads browser databases, cookies or credentials.
use std::io::{self, Read};
use std::path::{Component, Path};
use std::sync::Arc;

use browser_continuity::{Profile, Provider};
use tilo_files::native::atomic::{Privacy, StorageDirectory};

use super::account_sources::{Candidate, Origin, Provider as AccountProvider};
use super::browser_inventory::{
    BrowserKind, Capability, Installation, Measure, ProfileInventory, ServiceInventory,
    WebsiteInventory,
};
use super::browser_selection::pinned::PinnedDirectory;
use super::nofollow::metadata_nofollow;

const MAX_PROFILE_DIRECTORIES: usize = 128;
const MAX_INI: u64 = 64 * 1024;
#[cfg(target_os = "linux")]
const MAX_DATABASE_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(target_os = "linux")]
const MAX_LOGINS_BYTES: u64 = 16 * 1024 * 1024;

pub struct ReviewedProfile {
    pub profile: Profile,
    pub label: String,
    pub browser: BrowserKind,
    pub installation: Installation,
    /// Whether this profile has a reviewed bookmarks/history export selection.
    /// A profile with only other browser data remains visible in the inventory
    /// but never becomes an empty authenticated plan entry.
    pub exportable: bool,
    directory: Arc<PinnedDirectory>,
}
impl ReviewedProfile {
    /// Revalidate the reviewed profile and its retained directory namespace.
    ///
    /// # Errors
    /// Refuses invalid profile metadata, replaced directories or unsafe namespace control.
    pub fn validate(&self) -> io::Result<()> {
        if self.exportable {
            self.profile.validate().map_err(invalid)?;
        }
        self.directory.validate()?;
        storage(&self.directory)?.validate_namespace_control()
    }
    /// Read a bounded, source-preserving inventory for this reviewed profile.
    /// Counts are proven against the same fixed source files the reviewed
    /// export path reads. Unsupported classes stay named instead of silently
    /// disappearing behind one browser total.
    ///
    /// # Errors
    /// Refuses changed or unsafe source authority. Individual unreadable data
    /// classes are reported as unknown rather than as an empty success.
    #[cfg(target_os = "linux")]
    pub fn inventory(&self) -> io::Result<ProfileInventory> {
        self.validate()?;
        let mut output = ProfileInventory::new(
            self.profile.id.clone(),
            self.browser,
            self.installation,
            self.browser.label().to_owned(),
            self.label.clone(),
        );
        let granted = browser_continuity::sources::GrantedProfile::from_directory(
            super::migration_durability::readable_directory(&self.directory.file)?,
        )?;
        match self.profile.provider {
            Provider::Chromium => {
                if regular(&self.directory, "Bookmarks")? {
                    if let Ok(bundle) =
                        browser_continuity::sources::parse_chromium_bookmarks_bundle(&granted)
                    {
                        output.bookmarks = Measure::Exact(bookmark_count(&bundle));
                        output.capabilities.bookmarks = Capability::Supported;
                    } else {
                        output.bookmarks = Measure::Unknown;
                        output.capabilities.bookmarks =
                            Capability::Unsupported("browser-source-unreadable");
                    }
                } else {
                    output.bookmarks = Measure::Exact(0);
                }
                if regular(&self.directory, "History")? {
                    if let Ok(bundle) =
                        browser_continuity::sources::parse_chromium_history_bundle(&granted)
                    {
                        output.history = Measure::Exact(history_count(&bundle));
                        output.capabilities.history = Capability::Supported;
                    } else {
                        output.history = Measure::Unknown;
                        output.capabilities.history =
                            Capability::Unsupported("browser-source-unreadable");
                    }
                } else {
                    output.history = Measure::Exact(0);
                }
            }
            Provider::Firefox => {
                if regular(&self.directory, "places.sqlite")? {
                    if let Ok(bundle) =
                        browser_continuity::sources::parse_firefox_places_selected_bundle(
                            &granted, true, false,
                        )
                    {
                        output.bookmarks = Measure::Exact(bookmark_count(&bundle));
                        output.capabilities.bookmarks = Capability::Supported;
                    } else {
                        output.bookmarks = Measure::Unknown;
                        output.capabilities.bookmarks =
                            Capability::Unsupported("browser-source-unreadable");
                    }
                    if let Ok(bundle) =
                        browser_continuity::sources::parse_firefox_places_selected_bundle(
                            &granted, false, true,
                        )
                    {
                        output.history = Measure::Exact(history_count(&bundle));
                        output.capabilities.history = Capability::Supported;
                    } else {
                        output.history = Measure::Unknown;
                        output.capabilities.history =
                            Capability::Unsupported("browser-source-unreadable");
                    }
                } else {
                    output.bookmarks = Measure::Exact(0);
                    output.history = Measure::Exact(0);
                }
            }
        }
        let (passwords, accounts) = login_counts(&self.directory, self.profile.provider)?;
        output.passwords = passwords;
        output.extensions = extension_count(&self.directory, self.profile.provider)?;
        output.settings = settings_count(&self.directory, self.profile.provider)?;
        if let Some(websites) = website_counts(&self.directory, self.profile.provider)? {
            output.websites = websites;
        }
        // Saved sign-ins are metadata: the distinct origins and user names in
        // the login store. Passwords stay encrypted in their source and the
        // export path never carries them, so this count never claims a working
        // sign-in was transferred.
        output.websites.accounts = accounts;
        output.websites.account_evidence = accounts.exact().is_some_and(|count| count > 0);
        output.exportable = self.exportable;
        output.included = self.exportable;
        self.validate()?;
        Ok(output)
    }

    /// Capture the selected domains from this reviewed profile.
    ///
    /// # Errors
    /// Refuses changed selections or sources, unsafe namespaces and unreadable snapshot data.
    #[cfg(any(test, feature = "test-support"))]
    pub fn capture_files(
        &self,
        selection: &Profile,
    ) -> io::Result<browser_continuity::snapshot::Snapshot> {
        self.capture_files_cancelled(selection, &std::sync::atomic::AtomicBool::new(false))
    }
    /// Capture selected domains while observing cancellation and source identity.
    ///
    /// # Errors
    /// Refuses cancellation, changed selections or sources, unsafe namespaces and capture failures.
    pub fn capture_files_cancelled(
        &self,
        selection: &Profile,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<browser_continuity::snapshot::Snapshot> {
        self.validate()?;
        if !self.exportable {
            return Err(invalid("Browser profile has no supported export domains"));
        }
        if selection.id != self.profile.id
            || selection.provider != self.profile.provider
            || selection.relative_path != self.profile.relative_path
            || (selection.bookmarks && !self.profile.bookmarks)
            || (selection.history && !self.profile.history)
        {
            return Err(invalid(
                "Browser selection changed; review its source again",
            ));
        }
        for attempt in 0..3 {
            super::browser_selection::check_cancel(cancel)?;
            self.validate()?;
            match browser_continuity::snapshot::capture_cancellable(
                super::migration_durability::readable_directory(&self.directory.file)?,
                selection,
                cancel,
            ) {
                Ok(captured) => {
                    super::browser_selection::check_cancel(cancel)?;
                    self.validate()?;
                    return Ok(captured);
                }
                Err(error)
                    if attempt < 2 && browser_continuity::snapshot::content_changed(&error) =>
                {
                    self.validate()?;
                    super::browser_selection::check_cancel(cancel)?;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => return Err(error),
            }
        }
        Err(invalid("Selected browser contents could not be captured"))
    }
}

/// Discover only recognized sources supplied by the consented account inventory.
/// Source roots outside the granted home and incomplete scans are refused.
///
/// # Errors
/// Refuses unsafe or changed directories, unrecognized sources, oversized scans and I/O failures.
pub fn discover(home: &Path, candidates: &[Candidate]) -> io::Result<Vec<ReviewedProfile>> {
    let root = PinnedDirectory::open_absolute(home)?;
    storage(&root)?.validate_namespace_control()?;
    let mut profiles = Vec::new();
    for candidate in candidates
        .iter()
        .filter(|item| item.origin == Origin::Browser)
    {
        let relative = components(candidate.profile.strip_prefix(home).map_err(invalid)?)?;
        let directory = walk(&root, &relative)?;
        let browser = super::browser_inventory::kind_for(candidate.provider, &candidate.label)
            .ok_or_else(|| invalid("Unrecognized browser source"))?;
        let installation = installation_for(home, &candidate.profile);
        match candidate.provider {
            AccountProvider::Chromium => chromium(
                &directory,
                &relative,
                &candidate.label,
                browser,
                installation,
                &mut profiles,
            )?,
            AccountProvider::Firefox => firefox(
                &directory,
                &relative,
                &candidate.label,
                browser,
                installation,
                &mut profiles,
            )?,
            _ => return Err(invalid("Unrecognized browser source")),
        }
        if profiles.len() > browser_continuity::MAX_PROFILES {
            return Err(invalid("Too many browser profiles to transfer"));
        }
    }
    root.validate()?;
    Ok(profiles)
}
fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
fn storage(pin: &PinnedDirectory) -> io::Result<StorageDirectory> {
    StorageDirectory::from_file(super::migration_durability::readable_directory(&pin.file)?)
}
fn components(path: &Path) -> io::Result<Vec<String>> {
    path.components()
        .map(|item| match item {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("Unsupported profile name")),
            _ => Err(invalid(
                "Browser profile must remain inside its granted source",
            )),
        })
        .collect()
}
fn walk(root: &Arc<PinnedDirectory>, names: &[String]) -> io::Result<Arc<PinnedDirectory>> {
    let mut directory = Arc::clone(root);
    for name in names {
        let metadata = metadata_nofollow(&directory.file, Path::new(name))?;
        directory = directory.open_child(name.as_ref(), &metadata)?;
        storage(&directory)?.validate_namespace_control()?;
    }
    Ok(directory)
}
fn regular(directory: &PinnedDirectory, name: &str) -> io::Result<bool> {
    match metadata_nofollow(&directory.file, Path::new(name)) {
        Ok(metadata) if metadata.is_file() && !metadata.is_symlink() => Ok(true),
        Ok(_) => Err(invalid("Browser data must be a regular source file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
fn regular_path(directory: &Arc<PinnedDirectory>, path: &str) -> io::Result<bool> {
    let mut parts = path.split('/').collect::<Vec<_>>();
    let leaf = parts
        .pop()
        .ok_or_else(|| invalid("Browser inventory path is empty"))?;
    let mut parent = Arc::clone(directory);
    for part in parts {
        let metadata = match metadata_nofollow(&parent.file, Path::new(part)) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if metadata.is_symlink() || !metadata.is_dir() {
            return Err(invalid("Browser profile data is redirected"));
        }
        parent = parent.open_child(part.as_ref(), &metadata)?;
    }
    regular(&parent, leaf)
}

fn directory_exists(directory: &Arc<PinnedDirectory>, name: &str) -> io::Result<bool> {
    match metadata_nofollow(&directory.file, Path::new(name)) {
        Ok(metadata) if metadata.is_dir() && !metadata.is_symlink() => Ok(true),
        Ok(_) => Err(invalid("Browser profile directory is redirected")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn has_chromium_data(directory: &Arc<PinnedDirectory>) -> io::Result<bool> {
    for path in [
        "Network/Cookies",
        "Cookies",
        "Login Data",
        "Preferences",
        "Secure Preferences",
    ] {
        if regular_path(directory, path)? {
            return Ok(true);
        }
    }
    directory_exists(directory, "Extensions")
}

fn has_firefox_data(directory: &Arc<PinnedDirectory>) -> io::Result<bool> {
    for name in ["cookies.sqlite", "logins.json", "prefs.js"] {
        if regular(directory, name)? {
            return Ok(true);
        }
    }
    directory_exists(directory, "extensions")
}

#[derive(Clone, Copy)]
struct SourceSelection {
    browser: BrowserKind,
    installation: Installation,
    exportable: bool,
}

fn add(
    directory: Arc<PinnedDirectory>,
    relative: Vec<String>,
    label: String,
    source: SourceSelection,
    provider: Provider,
    selected: (bool, bool),
    output: &mut Vec<ReviewedProfile>,
) -> io::Result<()> {
    let identity = serde_json::to_vec(&(provider, &relative)).map_err(invalid)?;
    let profile = Profile {
        id: blake3::hash(&identity).to_hex().to_string(),
        provider,
        relative_path: relative,
        bookmarks: selected.0,
        history: selected.1,
    };
    if source.exportable {
        profile.validate().map_err(invalid)?;
    }
    if output.iter().any(|item| item.profile.id == profile.id) {
        return Ok(());
    }
    output.push(ReviewedProfile {
        profile,
        label,
        browser: source.browser,
        installation: source.installation,
        exportable: source.exportable,
        directory,
    });
    Ok(())
}
fn chromium(
    root: &Arc<PinnedDirectory>,
    relative: &[String],
    label: &str,
    browser: BrowserKind,
    installation: Installation,
    output: &mut Vec<ReviewedProfile>,
) -> io::Result<()> {
    let entries = cap_std::fs::Dir::from_std_file(super::migration_durability::readable_directory(
        &root.file,
    )?);
    for (index, entry) in entries.entries()?.enumerate() {
        if index >= MAX_PROFILE_DIRECTORIES {
            return Err(invalid("Browser profile discovery exceeds its bound"));
        }
        let name = entry?.file_name();
        let metadata = metadata_nofollow(&root.file, Path::new(&name))?;
        if metadata.is_symlink() {
            if name == "Default"
                || name
                    .to_str()
                    .is_some_and(|value| value.starts_with("Profile "))
            {
                return Err(invalid("Browser profile is redirected"));
            }
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }
        let name = name
            .to_str()
            .ok_or_else(|| invalid("Unsupported profile name"))?
            .to_owned();
        let directory = walk(root, std::slice::from_ref(&name))?;
        let selected = (
            regular(&directory, "Bookmarks")?,
            regular(&directory, "History")?,
        );
        let exportable = selected.0 || selected.1;
        if !exportable && !has_chromium_data(&directory)? {
            continue;
        }
        let mut path = relative.to_vec();
        path.push(name.clone());
        add(
            directory,
            path,
            format!("{label} · {name}"),
            SourceSelection {
                browser,
                installation,
                exportable,
            },
            Provider::Chromium,
            selected,
            output,
        )?;
    }
    root.validate()
}
fn firefox(
    root: &Arc<PinnedDirectory>,
    relative: &[String],
    label: &str,
    browser: BrowserKind,
    installation: Installation,
    output: &mut Vec<ReviewedProfile>,
) -> io::Result<()> {
    let mut file = storage(root)?.open_read("profiles.ini".as_ref(), Privacy::Inherit)?;
    if file.metadata()?.len() > MAX_INI {
        return Err(invalid("Firefox profile index exceeds its bound"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_INI + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INI {
        return Err(invalid("Firefox profile index exceeds its bound"));
    }
    let content = std::str::from_utf8(&bytes).map_err(invalid)?;
    let sections = firefox_sections(content)?;
    for (name, path) in sections {
        let names = components(Path::new(&path))?;
        let directory = walk(root, &names)?;
        let exportable = regular(&directory, "places.sqlite")?;
        if !exportable && !has_firefox_data(&directory)? {
            continue;
        }
        let mut path = relative.to_vec();
        path.extend(names);
        add(
            directory,
            path,
            format!("{label} · {name}"),
            SourceSelection {
                browser,
                installation,
                exportable,
            },
            Provider::Firefox,
            (exportable, exportable),
            output,
        )?;
    }
    root.validate()
}
fn firefox_sections(content: &str) -> io::Result<Vec<(String, String)>> {
    let mut output = Vec::new();
    let mut active = false;
    let mut name = None;
    let mut path = None;
    let mut relative = None;
    let finish = |active,
                  name: &mut Option<String>,
                  path: &mut Option<String>,
                  relative: &mut Option<bool>,
                  output: &mut Vec<(String, String)>|
     -> io::Result<()> {
        if active {
            if relative.take() != Some(true) {
                return Err(invalid("Firefox profile lies outside its granted source"));
            }
            output.push((
                name.take().unwrap_or_default(),
                path.take()
                    .ok_or_else(|| invalid("Firefox profile has no path"))?,
            ));
            if output.len() > browser_continuity::MAX_PROFILES {
                return Err(invalid("Too many Firefox profiles"));
            }
        }
        name.take();
        path.take();
        relative.take();
        Ok(())
    };
    for line in content.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') {
            finish(active, &mut name, &mut path, &mut relative, &mut output)?;
            active = line.starts_with("[Profile");
        } else if active && let Some((key, value)) = line.split_once('=') {
            let value = value.trim();
            match key.trim() {
                "Name" => name = Some(value.into()),
                "Path" => path = Some(value.into()),
                "IsRelative" => relative = Some(value == "1"),
                _ => {}
            }
        }
    }
    finish(active, &mut name, &mut path, &mut relative, &mut output)?;
    Ok(output)
}

fn installation_for(home: &Path, profile: &Path) -> Installation {
    let relative = profile.strip_prefix(home).unwrap_or(profile);
    if relative.starts_with(".var/app") {
        Installation::Flatpak
    } else {
        Installation::Native
    }
}

#[cfg(target_os = "linux")]
fn bookmark_count(bundle: &browser_continuity::imports::ImportBundle) -> u64 {
    bundle.chunks.iter().fold(0u64, |total, chunk| {
        total.saturating_add(u64::try_from(chunk.bookmarks.len()).unwrap_or(u64::MAX))
    })
}

#[cfg(target_os = "linux")]
fn history_count(bundle: &browser_continuity::imports::ImportBundle) -> u64 {
    bundle.chunks.iter().fold(0u64, |total, chunk| {
        total.saturating_add(u64::try_from(chunk.history.len()).unwrap_or(u64::MAX))
    })
}

#[cfg(target_os = "linux")]
fn read_bounded(
    directory: &PinnedDirectory,
    name: &str,
    limit: u64,
) -> io::Result<Option<Vec<u8>>> {
    let mut file = match storage(directory)?.open_read(name.as_ref(), Privacy::Inherit) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > limit {
        return Err(invalid("Browser inventory source exceeds its bound"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(invalid("Browser inventory source exceeds its bound"));
    }
    Ok(Some(bytes))
}

#[cfg(target_os = "linux")]
fn locate_file(
    directory: &Arc<PinnedDirectory>,
    path: &str,
) -> io::Result<Option<(Arc<PinnedDirectory>, String)>> {
    let mut parts = path.split('/').collect::<Vec<_>>();
    let leaf = parts
        .pop()
        .ok_or_else(|| invalid("Browser inventory path is empty"))?;
    let mut parent = Arc::clone(directory);
    for part in parts {
        parent = walk(&parent, &[part.to_owned()])?;
    }
    match metadata_nofollow(&parent.file, Path::new(leaf)) {
        Ok(metadata) if metadata.is_file() && !metadata.is_symlink() => {
            Ok(Some((parent, leaf.to_owned())))
        }
        Ok(_) => Err(invalid(
            "Browser inventory data must be a regular source file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
struct PrivateDatabase {
    _temporary: tempfile::TempDir,
    connection: rusqlite::Connection,
}

#[cfg(target_os = "linux")]
fn open_database(
    directory: &Arc<PinnedDirectory>,
    candidates: &[&str],
) -> io::Result<Option<PrivateDatabase>> {
    let mut located = None;
    for candidate in candidates {
        match locate_file(directory, candidate) {
            Ok(Some(found)) => {
                located = Some(found);
                break;
            }
            Ok(None) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let Some((parent, name)) = located else {
        return Ok(None);
    };
    let database = read_bounded(&parent, &name, MAX_DATABASE_BYTES)?
        .ok_or_else(|| invalid("Browser inventory database disappeared"))?;
    let wal_name = format!("{name}-wal");
    let wal = read_bounded(&parent, &wal_name, MAX_DATABASE_BYTES)?;
    let temporary = tempfile::tempdir()?;
    std::fs::write(temporary.path().join(&name), &database)?;
    if let Some(wal) = wal {
        std::fs::write(temporary.path().join(&wal_name), &wal)?;
    }
    let connection = rusqlite::Connection::open_with_flags(
        temporary.path().join(&name),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite)?;
    connection
        .execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;")
        .map_err(sqlite)?;
    Ok(Some(PrivateDatabase {
        _temporary: temporary,
        connection,
    }))
}

#[cfg(target_os = "linux")]
fn sqlite(_: rusqlite::Error) -> io::Error {
    invalid("Browser inventory database is unreadable or incompatible")
}

#[cfg(target_os = "linux")]
fn query_pair(connection: &rusqlite::Connection, sql: &str) -> io::Result<(u64, u64)> {
    let (first, second): (i64, i64) = connection
        .query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(sqlite)?;
    Ok((
        u64::try_from(first).map_err(|_| invalid("Browser inventory count is invalid"))?,
        u64::try_from(second).map_err(|_| invalid("Browser inventory count is invalid"))?,
    ))
}

#[cfg(target_os = "linux")]
fn query_count(connection: &rusqlite::Connection, sql: &str) -> io::Result<u64> {
    let count: i64 = connection
        .query_row(sql, [], |row| row.get(0))
        .map_err(sqlite)?;
    u64::try_from(count).map_err(|_| invalid("Browser inventory count is invalid"))
}

#[cfg(target_os = "linux")]
fn website_counts(
    directory: &Arc<PinnedDirectory>,
    provider: Provider,
) -> io::Result<Option<WebsiteInventory>> {
    let (candidates, table, host) = match provider {
        Provider::Chromium => (&["Network/Cookies", "Cookies"][..], "cookies", "host_key"),
        Provider::Firefox => (&["cookies.sqlite"][..], "moz_cookies", "host"),
    };
    let Some(database) = open_database(directory, candidates)? else {
        return Ok(None);
    };
    let (cookies, origins) = query_pair(
        &database.connection,
        &format!("SELECT COUNT(*), COUNT(DISTINCT {host}) FROM {table}"),
    )?;
    let mut inventory = WebsiteInventory {
        cookies: Measure::Exact(cookies),
        origins: Measure::Exact(origins),
        ..WebsiteInventory::default()
    };
    for service in [
        browser_continuity::sessions::Service::Slack,
        browser_continuity::sessions::Service::Discord,
    ] {
        let base = match service {
            browser_continuity::sessions::Service::Slack => "slack.com",
            browser_continuity::sessions::Service::Discord => "discord.com",
        };
        let (cookies, origins) = query_pair(
            &database.connection,
            &format!(
                "SELECT COUNT(*), COUNT(DISTINCT {host}) FROM {table} WHERE lower({host})='{base}' OR lower({host}) LIKE '%.{base}'"
            ),
        )?;
        if cookies > 0 {
            inventory.services.push(ServiceInventory {
                service,
                cookies: Measure::Exact(cookies),
                origins: Measure::Exact(origins),
                account_evidence: false,
            });
        }
    }
    Ok(Some(inventory))
}

#[cfg(target_os = "linux")]
fn login_counts(
    directory: &Arc<PinnedDirectory>,
    provider: Provider,
) -> io::Result<(Measure, Measure)> {
    match provider {
        Provider::Chromium => match open_database(directory, &["Login Data"])? {
            Some(database) => {
                let passwords = query_count(&database.connection, "SELECT COUNT(*) FROM logins")?;
                // Only the origin and user name are read. The encrypted value
                // stays in the source snapshot and never enters this process.
                let accounts = query_count(
                    &database.connection,
                    "SELECT COUNT(DISTINCT origin_url || char(0) || username_value) \
                     FROM logins WHERE length(trim(username_value)) > 0",
                )?;
                Ok((Measure::Exact(passwords), Measure::Exact(accounts)))
            }
            None => Ok((Measure::Exact(0), Measure::Exact(0))),
        },
        Provider::Firefox => {
            let Some(bytes) = read_bounded(directory, "logins.json", MAX_LOGINS_BYTES)? else {
                return Ok((Measure::Exact(0), Measure::Exact(0)));
            };
            let document: serde_json::Value = match serde_json::from_slice(&bytes) {
                Ok(document) => document,
                Err(_) => return Ok((Measure::Unknown, Measure::Unknown)),
            };
            let Some(logins) = document.get("logins").and_then(serde_json::Value::as_array) else {
                return Ok((Measure::Unknown, Measure::Unknown));
            };
            let mut accounts = std::collections::BTreeSet::new();
            for login in logins {
                let username = login
                    .get("username")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .trim();
                if username.is_empty() {
                    continue;
                }
                let origin = login
                    .get("hostname")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                accounts.insert((origin.to_owned(), username.to_owned()));
            }
            Ok((
                Measure::Exact(u64::try_from(logins.len()).unwrap_or(u64::MAX)),
                Measure::Exact(u64::try_from(accounts.len()).unwrap_or(u64::MAX)),
            ))
        }
    }
}

#[cfg(target_os = "linux")]
fn entry_count(directory: &Arc<PinnedDirectory>, name: &str) -> io::Result<Option<u64>> {
    let metadata = match metadata_nofollow(&directory.file, Path::new(name)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.is_symlink() || !metadata.is_dir() {
        return Err(invalid("Browser inventory directory is redirected"));
    }
    let child = directory.open_child(name.as_ref(), &metadata)?;
    let entries = cap_std::fs::Dir::from_std_file(super::migration_durability::readable_directory(
        &child.file,
    )?);
    let mut count = 0u64;
    for entry in entries.entries()? {
        entry?;
        count = count.saturating_add(1);
        if count > u64::try_from(MAX_PROFILE_DIRECTORIES).unwrap_or(u64::MAX) {
            return Err(invalid("Browser inventory directory exceeds its bound"));
        }
    }
    Ok(Some(count))
}

#[cfg(target_os = "linux")]
fn extension_count(directory: &Arc<PinnedDirectory>, provider: Provider) -> io::Result<Measure> {
    let name = match provider {
        Provider::Chromium => "Extensions",
        Provider::Firefox => "extensions",
    };
    Ok(match entry_count(directory, name)? {
        Some(count) => Measure::Exact(count),
        None => Measure::Exact(0),
    })
}

#[cfg(target_os = "linux")]
fn settings_count(directory: &Arc<PinnedDirectory>, provider: Provider) -> io::Result<Measure> {
    let names: &[&str] = match provider {
        Provider::Chromium => &["Preferences", "Secure Preferences"],
        Provider::Firefox => &["prefs.js"],
    };
    let mut count = 0u64;
    for name in names {
        if regular(directory, name)? {
            count = count.saturating_add(1);
        }
    }
    Ok(Measure::Exact(count))
}

#[cfg(not(target_os = "linux"))]
impl ReviewedProfile {
    /// Inventory inspection is unavailable where the native database reader is not built.
    ///
    /// # Errors
    /// Returns `Unsupported` rather than an empty complete inventory.
    pub fn inventory(&self) -> io::Result<ProfileInventory> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Browser inventory is unavailable on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn fixture() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join(".config/chromium/Default")).unwrap();
        fs::create_dir_all(home.path().join(".mozilla/firefox/first.default")).unwrap();
        home
    }
    fn inventory(home: &Path) -> super::super::account_sources::Inventory {
        super::super::account_sources::discover(&super::super::account_sources::SourceRoots {
            home: home.into(),
            config: home.join(".config"),
        })
    }
    #[test]
    fn actual_granted_browser_discovery_selects_exact_profiles_without_reading_databases() {
        let home = fixture();
        let chrome = home.path().join(".config/chromium");
        let history = chrome.join("Default/History");
        fs::write(
            &history,
            b"not a SQLite database; discovery must not parse it",
        )
        .unwrap();
        fs::write(chrome.join("Default/Bookmarks"), b"not JSON; metadata only").unwrap();
        symlink("not-a-directory", chrome.join("SingletonLock")).unwrap();
        let firefox = home.path().join(".mozilla/firefox");
        let index = b"[Profile0]\nName=Personal\nIsRelative=1\nPath=first.default\n";
        fs::write(firefox.join("profiles.ini"), index).unwrap();
        fs::write(
            firefox.join("first.default/places.sqlite"),
            b"unparsed private data",
        )
        .unwrap();
        let before = fs::metadata(&history).unwrap();
        let profiles = discover(home.path(), &inventory(home.path()).candidates).unwrap();
        assert_eq!(profiles.len(), 2);
        assert!(
            profiles
                .iter()
                .all(|item| item.profile.bookmarks && item.profile.history)
        );
        assert!(
            profiles
                .iter()
                .any(|item| item.profile.relative_path == [".config", "chromium", "Default"])
        );
        assert!(
            profiles
                .iter()
                .any(|item| item.profile.relative_path == [".mozilla", "firefox", "first.default"])
        );
        for profile in &profiles {
            profile.validate().unwrap();
        }
        assert_eq!(
            fs::read(&history).unwrap(),
            b"not a SQLite database; discovery must not parse it"
        );
        assert_eq!(
            fs::metadata(&history).unwrap().permissions().mode(),
            before.permissions().mode()
        );
        assert_eq!(fs::read(firefox.join("profiles.ini")).unwrap(), index);
        fs::rename(chrome.join("Default"), chrome.join("old-default")).unwrap();
        fs::create_dir(chrome.join("Default")).unwrap();
        assert!(
            profiles
                .iter()
                .find(|item| item.profile.provider == Provider::Chromium)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    #[test]
    fn redirected_or_external_profiles_are_not_silently_discovered() {
        let home = fixture();
        let outside = tempfile::tempdir().unwrap();
        let chrome = home.path().join(".config/chromium");
        fs::remove_dir(chrome.join("Default")).unwrap();
        symlink(outside.path(), chrome.join("Default")).unwrap();
        let before: Vec<_> = fs::read_dir(outside.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(discover(home.path(), &inventory(home.path()).candidates).is_err());
        assert_eq!(
            fs::read_dir(outside.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            before
        );
        fs::remove_file(chrome.join("Default")).unwrap();
        let firefox = home.path().join(".mozilla/firefox");
        fs::write(
            firefox.join("profiles.ini"),
            "[Profile0]\nName=External\nIsRelative=0\nPath=/elsewhere\n",
        )
        .unwrap();
        assert!(discover(home.path(), &inventory(home.path()).candidates).is_err());
    }
}
