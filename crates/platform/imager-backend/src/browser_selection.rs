//! Independent, consented browser domains; never inferred from file/account choices.
use std::io;
use std::path::Path;
use std::sync::Arc;

// Portable selection metadata shared with presentation clients. Native source,
// activation and completion authority remains in backend actors.
pub use browser_continuity::{PROFILES_DIRECTORY, Plan, Profile, Provider, sessions::Service};

#[cfg(unix)]
pub use browser_continuity::snapshot::{CapturedFile, Snapshot};
#[cfg(not(unix))]
pub use unsupported_capture::{CapturedFile, Snapshot};
#[cfg(not(unix))]
mod unsupported_capture {
    /// Portable read projection; unsupported backends cannot construct one.
    pub struct CapturedFile {
        name: &'static str,
        bytes: Vec<u8>,
    }
    impl CapturedFile {
        pub fn name(&self) -> &'static str {
            self.name
        }
        pub fn bytes(&self) -> &[u8] {
            &self.bytes
        }
    }
    pub struct Snapshot {
        files: Vec<CapturedFile>,
        fingerprint: [u8; 32],
    }
    impl Snapshot {
        pub fn files(&self) -> &[CapturedFile] {
            &self.files
        }
        pub fn fingerprint(&self) -> [u8; 32] {
            self.fingerprint
        }
        pub fn bytes(&self) -> u64 {
            self.files.iter().map(|file| file.bytes.len() as u64).sum()
        }
    }
}

use crate::account_sources::Candidate;
use crate::browser_inventory::{ProfileInventory, Report};
use crate::browser_profiles::{self, ReviewedProfile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    Bookmarks,
    History,
}
#[derive(Clone, Default)]
pub struct SelectedSources {
    profiles: Vec<(Arc<ReviewedProfile>, Profile)>,
}
impl SelectedSources {
    /// Select only domains present in distinct reviewed browser profiles.
    ///
    /// # Errors
    /// Refuses invalid profiles, widened or changed selections, duplicates and excessive counts.
    pub fn from_reviewed(profiles: Vec<(Arc<ReviewedProfile>, Profile)>) -> io::Result<Self> {
        if profiles.len() > browser_continuity::MAX_PROFILES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Too many selected browser sources",
            ));
        }
        for (index, (source, selected)) in profiles.iter().enumerate() {
            selected.validate().map_err(io::Error::other)?;
            if source.profile.id != selected.id
                || source.profile.provider != selected.provider
                || source.profile.relative_path != selected.relative_path
                || (selected.bookmarks && !source.profile.bookmarks)
                || (selected.history && !source.profile.history)
                || profiles
                    .iter()
                    .take(index)
                    .any(|(_, prior)| prior.id == selected.id)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Browser selection differs from its reviewed source",
                ));
            }
        }
        Ok(Self { profiles })
    }
    pub fn profiles(&self) -> &[(Arc<ReviewedProfile>, Profile)] {
        &self.profiles
    }
    /// Bind the selected profile metadata to an authenticated transfer identity.
    ///
    /// # Errors
    /// Refuses an invalid binding, identity or profile set.
    pub fn plan(&self, binding: String, identity: [u8; 32]) -> io::Result<Plan> {
        Plan::new(
            binding,
            identity,
            self.profiles
                .iter()
                .map(|(_, profile)| profile.clone())
                .collect(),
        )
        .map_err(io::Error::other)
    }
}
/// One profile's own inclusion and domain decisions. An exclusion clears both
/// domains, so the authenticated plan cannot carry part of a profile that the
/// review excluded. Re-including restores the domains its reviewed source
/// supports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProfileState {
    pub exportable: bool,
    pub included: bool,
    pub bookmarks: bool,
    pub history: bool,
}
impl ProfileState {
    #[must_use]
    pub const fn any_selected(self) -> bool {
        self.included && (self.bookmarks || self.history)
    }
}

pub struct Selection {
    profiles: Vec<Arc<ReviewedProfile>>,
    inventory: Vec<ProfileInventory>,
    inventory_incomplete: bool,
    choices: std::collections::BTreeMap<String, ProfileState>,
    bookmarks: bool,
    history: bool,
}
impl Default for Selection {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            inventory: Vec::new(),
            inventory_incomplete: false,
            choices: std::collections::BTreeMap::new(),
            bookmarks: true,
            history: true,
        }
    }
}
impl Selection {
    /// The caller runs this on its consented discovery worker, not the UI thread.
    ///
    /// # Errors
    /// Propagates source discovery failures, including unsafe directories and oversized scans.
    pub fn discover(home: &Path, candidates: &[Candidate]) -> io::Result<Self> {
        let profiles = browser_profiles::discover(home, candidates)?
            .into_iter()
            .map(Arc::new)
            .collect::<Vec<_>>();
        let mut inventory = Vec::new();
        let mut inventory_incomplete = false;
        for source in &profiles {
            if let Ok(item) = source.inventory() {
                inventory.push(item);
            } else {
                inventory_incomplete = true;
                let mut item = ProfileInventory::new(
                    source.profile.id.clone(),
                    source.browser,
                    source.installation,
                    source.browser.label().to_owned(),
                    source.label.clone(),
                );
                item.capabilities.bookmarks = crate::browser_inventory::Capability::Unsupported(
                    "browser-inventory-unavailable",
                );
                item.capabilities.history = crate::browser_inventory::Capability::Unsupported(
                    "browser-inventory-unavailable",
                );
                item.exportable = source.exportable;
                item.bookmarks = crate::browser_inventory::Measure::Unknown;
                item.history = crate::browser_inventory::Measure::Unknown;
                inventory.push(item);
            }
        }
        let choices = profiles
            .iter()
            .map(|source| {
                (
                    source.profile.id.clone(),
                    ProfileState {
                        exportable: source.exportable,
                        included: true,
                        bookmarks: source.profile.bookmarks,
                        history: source.profile.history,
                    },
                )
            })
            .collect();
        Ok(Self {
            profiles,
            inventory,
            inventory_incomplete,
            choices,
            ..Self::default()
        })
    }
    /// Source-oriented metadata for the review worker. No path or private value.
    #[must_use]
    pub fn inventory(&self) -> &[ProfileInventory] {
        &self.inventory
    }
    /// True when at least one profile could not be inspected completely.
    #[must_use]
    pub fn inventory_incomplete(&self) -> bool {
        self.inventory_incomplete || self.inventory.iter().any(ProfileInventory::incomplete)
    }
    /// Bounded report with the current per-profile domain selection.
    #[must_use]
    pub fn inventory_report(&self) -> Report {
        let mut report = Report {
            schema: crate::browser_inventory::SCHEMA,
            profiles: self.inventory.clone(),
            incomplete: self.inventory_incomplete(),
        };
        for profile in &mut report.profiles {
            let state = self.choice(&profile.id);
            profile.included = profile.exportable && state.any_selected();
            profile.bookmarks_selected = profile.exportable && state.included && state.bookmarks;
            profile.history_selected = profile.exportable && state.included && state.history;
        }
        report
    }
    fn choice(&self, id: &str) -> ProfileState {
        self.choices.get(id).copied().unwrap_or_default()
    }
    /// One reviewed profile's own inclusion and domain decisions.
    #[must_use]
    pub fn profile_state(&self, id: &str) -> Option<ProfileState> {
        self.choices.get(id).copied()
    }
    /// Snapshot every profile decision for an atomic batch that may fail.
    #[must_use]
    pub fn profile_states(&self) -> Vec<(String, ProfileState)> {
        self.profiles
            .iter()
            .filter_map(|source| {
                self.profile_state(&source.profile.id)
                    .map(|state| (source.profile.id.clone(), state))
            })
            .collect()
    }
    /// Restore a snapshot taken from this same reviewed selection. Never widens
    /// or invents a source: an identity this selection no longer has stays out.
    pub fn restore_states(&mut self, states: &[(String, ProfileState)]) {
        for (id, state) in states {
            if let Some(choice) = self.choices.get_mut(id) {
                *choice = *state;
            }
        }
    }
    /// Carry the person's decisions onto a fresh discovery of the same granted
    /// home. A profile that disappeared cannot be recreated; a new profile keeps
    /// the discovery default.
    pub fn adopt(&mut self, previous: &Self) {
        for (id, state) in &previous.choices {
            if let Some(choice) = self.choices.get_mut(id) {
                *choice = *state;
            }
        }
        self.bookmarks = previous.bookmarks;
        self.history = previous.history;
    }
    /// Exclude or restore one reviewed profile. Excluding clears every domain so
    /// no part of the profile reaches the plan, and journeys clear the profile's
    /// website-session choices in the same edit. Restoring puts back the domains
    /// its reviewed source supports.
    ///
    /// # Errors
    /// Refuses an unknown, replaced or no-longer-valid reviewed source.
    pub fn set_profile(&mut self, id: &str, included: bool) -> Result<(), &'static str> {
        let source = self
            .profiles
            .iter()
            .find(|source| source.profile.id == id)
            .ok_or("installer-browser-source-changed")?;
        source
            .validate()
            .map_err(|_| "installer-browser-source-changed")?;
        if included && !source.exportable {
            return Err("installer-browser-source-not-exportable");
        }
        let choice = self
            .choices
            .get_mut(id)
            .ok_or("installer-browser-source-changed")?;
        choice.included = included;
        if included {
            choice.bookmarks = source.profile.bookmarks;
            choice.history = source.profile.history;
        } else {
            choice.bookmarks = false;
            choice.history = false;
        }
        Ok(())
    }
    /// Set one profile's domain without touching any other profile.
    ///
    /// # Errors
    /// Refuses an unknown or invalid source and a domain its reviewed profile
    /// does not have.
    pub fn set_profile_domain(
        &mut self,
        id: &str,
        domain: Domain,
        included: bool,
    ) -> Result<bool, &'static str> {
        let source = self
            .profiles
            .iter()
            .find(|source| source.profile.id == id)
            .ok_or("installer-browser-source-changed")?;
        source
            .validate()
            .map_err(|_| "installer-browser-source-changed")?;
        let supported = match domain {
            Domain::Bookmarks => source.profile.bookmarks,
            Domain::History => source.profile.history,
        };
        if included && (!source.exportable || !supported) {
            return Err("installer-browser-source-not-exportable");
        }
        let choice = self
            .choices
            .get_mut(id)
            .ok_or("installer-browser-source-changed")?;
        let previous = *choice;
        match domain {
            Domain::Bookmarks => choice.bookmarks = included,
            Domain::History => choice.history = included,
        }
        if included {
            choice.included = true;
        }
        Ok(*choice != previous)
    }
    /// Whether the current selection carries this reviewed profile.
    #[must_use]
    pub fn profile_included(&self, id: &str) -> bool {
        self.choice(id).included
    }
    pub fn available(&self) -> bool {
        !self.profiles.is_empty()
    }
    /// Whether any included profile carries this domain. The global control that
    /// reads this still changes every profile; each profile's own item is the
    /// authority for its own domain.
    pub fn chosen(&self, domain: Domain) -> bool {
        if self.profiles.is_empty() {
            return match domain {
                Domain::Bookmarks => self.bookmarks,
                Domain::History => self.history,
            };
        }
        self.profiles.iter().any(|source| {
            let state = self.choice(&source.profile.id);
            state.included
                && match domain {
                    Domain::Bookmarks => state.bookmarks,
                    Domain::History => state.history,
                }
        })
    }
    /// Set one domain on every reviewed profile that has it. This is the legacy
    /// all-profiles control; a per-profile item uses `set_profile_domain`.
    pub fn set(&mut self, domain: Domain, included: bool) {
        match domain {
            Domain::Bookmarks => self.bookmarks = included,
            Domain::History => self.history = included,
        }
        for source in &self.profiles {
            let supported = match domain {
                Domain::Bookmarks => source.profile.bookmarks,
                Domain::History => source.profile.history,
            };
            if !supported {
                continue;
            }
            if let Some(choice) = self.choices.get_mut(&source.profile.id) {
                match domain {
                    Domain::Bookmarks => choice.bookmarks = included,
                    Domain::History => choice.history = included,
                }
            }
        }
    }
    pub fn selected(&self) -> SelectedSources {
        SelectedSources {
            profiles: self
                .profiles
                .iter()
                .filter_map(|source| {
                    let state = self.choice(&source.profile.id);
                    if !source.exportable || !state.included {
                        return None;
                    }
                    let mut profile = source.profile.clone();
                    profile.bookmarks &= state.bookmarks;
                    profile.history &= state.history;
                    (profile.bookmarks || profile.history).then(|| (Arc::clone(source), profile))
                })
                .collect(),
        }
    }
    /// Reviewed metadata for presentation; not a source constructor.
    pub fn profiles(&self) -> impl Iterator<Item = &ReviewedProfile> {
        self.profiles.iter().map(AsRef::as_ref)
    }
}

#[cfg(unix)]
pub(crate) mod pinned;

#[cfg(unix)]
pub(crate) fn check_cancel(cancel: &std::sync::atomic::AtomicBool) -> io::Result<()> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        Err(io::Error::new(io::ErrorKind::Interrupted, "copy cancelled"))
    } else {
        Ok(())
    }
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(target_os = "linux")]
    fn chromium_profile(
        directory: &std::path::Path,
        bookmark: &str,
        history: &str,
        cookie_hosts: &[&str],
    ) -> std::io::Result<()> {
        fs::create_dir_all(directory.join("Network"))?;
        fs::create_dir_all(directory.join("Extensions/example-extension"))?;
        fs::write(
            directory.join("Bookmarks"),
            format!(
                "{{\"roots\":{{\"bookmark_bar\":{{\"type\":\"folder\",\"name\":\"Bar\",\"children\":[{{\"type\":\"url\",\"name\":\"{bookmark}\",\"url\":\"https://example.com/{bookmark}\"}}]}}}}}}"
            ),
        )?;
        let database =
            rusqlite::Connection::open(directory.join("History")).map_err(std::io::Error::other)?;
        database
            .execute_batch(
                "CREATE TABLE urls(url TEXT, title TEXT, last_visit_time INTEGER, visit_count INTEGER);",
            )
            .map_err(std::io::Error::other)?;
        database
            .execute(
                "INSERT INTO urls VALUES(?1, ?2, ?3, 2)",
                rusqlite::params![
                    format!("https://example.com/{history}"),
                    history,
                    13_300_000_000_000_000_i64
                ],
            )
            .map_err(std::io::Error::other)?;
        drop(database);
        let cookies = rusqlite::Connection::open(directory.join("Network/Cookies"))
            .map_err(std::io::Error::other)?;
        cookies
            .execute_batch(
                "CREATE TABLE meta(key TEXT, value TEXT);
                 INSERT INTO meta VALUES('version','24');
                 CREATE TABLE cookies(host_key TEXT, value TEXT);",
            )
            .map_err(std::io::Error::other)?;
        for host in cookie_hosts {
            cookies
                .execute(
                    "INSERT INTO cookies VALUES(?1, 'PRIVATE_COOKIE_SENTINEL')",
                    [host],
                )
                .map_err(std::io::Error::other)?;
        }
        drop(cookies);
        let logins = rusqlite::Connection::open(directory.join("Login Data"))
            .map_err(std::io::Error::other)?;
        logins
            .execute_batch(
                "CREATE TABLE logins(origin_url TEXT, username_value TEXT, password_value BLOB);
                 INSERT INTO logins VALUES('https://example.com', 'person@example.com', 'PRIVATE_PASSWORD_SENTINEL');",
            )
            .map_err(std::io::Error::other)?;
        drop(logins);
        fs::write(directory.join("Preferences"), "{}")?;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn firefox_profile(
        directory: &std::path::Path,
        bookmark: &str,
        history: &str,
        host: &str,
    ) -> std::io::Result<()> {
        fs::create_dir_all(directory.join("extensions/example-extension"))?;
        let places = rusqlite::Connection::open(directory.join("places.sqlite"))
            .map_err(std::io::Error::other)?;
        places
            .execute_batch(
                "CREATE TABLE moz_places(id INTEGER PRIMARY KEY, url TEXT, title TEXT, last_visit_date INTEGER, visit_count INTEGER);
                 CREATE TABLE moz_bookmarks(id INTEGER PRIMARY KEY, parent INTEGER, position INTEGER, title TEXT, type INTEGER, fk INTEGER);
                 INSERT INTO moz_places VALUES(1, 'https://example.com/places', 'Places', 1700000000000000, 2);
                 INSERT INTO moz_bookmarks VALUES(1, 0, 0, 'Root', 2, NULL);
                 INSERT INTO moz_bookmarks VALUES(2, 1, 0, 'Bookmark', 1, 1);",
            )
            .map_err(std::io::Error::other)?;
        drop(places);
        let cookies = rusqlite::Connection::open(directory.join("cookies.sqlite"))
            .map_err(std::io::Error::other)?;
        cookies
            .execute_batch(
                "PRAGMA user_version=17;
                 CREATE TABLE moz_cookies(host TEXT, value TEXT);",
            )
            .map_err(std::io::Error::other)?;
        cookies
            .execute(
                "INSERT INTO moz_cookies VALUES(?1, 'PRIVATE_COOKIE_SENTINEL')",
                [host],
            )
            .map_err(std::io::Error::other)?;
        drop(cookies);
        fs::write(
            directory.join("logins.json"),
            "{\"logins\":[{\"hostname\":\"https://example.com\",\"username\":\"person@example.com\",\"password\":\"PRIVATE_PASSWORD_SENTINEL\"}]}",
        )?;
        fs::write(directory.join("prefs.js"), "user_pref(\"x\", 1);")?;
        let _ = (bookmark, history);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn firefox_root(
        home: &std::path::Path,
        root: &str,
        name: &str,
    ) -> std::io::Result<std::path::PathBuf> {
        let directory = home.join(root).join(name);
        fs::create_dir_all(&directory)?;
        fs::write(
            home.join(root).join("profiles.ini"),
            format!("[Profile0]\nName={name}\nIsRelative=1\nPath={name}\n"),
        )?;
        Ok(directory)
    }

    #[cfg(target_os = "linux")]
    fn inventory_fixture()
    -> std::io::Result<(tempfile::TempDir, Vec<(std::path::PathBuf, Vec<u8>, u32)>)> {
        let home = tempfile::tempdir()?;
        let chrome = home.path().join(".config/google-chrome/Default");
        let chromium = home.path().join(".config/chromium/Default");
        let ungoogled = home.path().join(".config/ungoogled-chromium/Default");
        let flatpak_chrome = home
            .path()
            .join(".var/app/com.google.Chrome/config/google-chrome/Default");
        for directory in [&chrome, &chromium, &ungoogled, &flatpak_chrome] {
            chromium_profile(
                directory,
                "Bookmark",
                "History",
                &["slack.com", "example.com"],
            )?;
        }
        let firefox = firefox_root(home.path(), ".mozilla/firefox", "first.default")?;
        firefox_profile(&firefox, "Bookmark", "History", "discord.com")?;
        let librewolf = firefox_root(home.path(), ".librewolf", "personal.default")?;
        firefox_profile(&librewolf, "Bookmark", "History", "example.com")?;
        let flatpak_librewolf = firefox_root(
            home.path(),
            ".var/app/io.gitlab.librewolf-community/.librewolf",
            "flatpak.default",
        )?;
        firefox_profile(&flatpak_librewolf, "Bookmark", "History", "example.com")?;
        let before = [
            chrome.join("Bookmarks"),
            chromium.join("History"),
            firefox.join("places.sqlite"),
            librewolf.join("cookies.sqlite"),
        ]
        .into_iter()
        .map(|path| {
            Ok((
                path.clone(),
                fs::read(&path)?,
                fs::metadata(&path)?.permissions().mode(),
            ))
        })
        .collect::<std::io::Result<Vec<_>>>()?;
        Ok((home, before))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_inventory_names_each_browser_and_counts_each_data_class() {
        let (home, _) = inventory_fixture().unwrap();
        let candidates = crate::account_sources::discover(&crate::account_sources::SourceRoots {
            home: home.path().into(),
            config: home.path().join(".config"),
        });
        let selection = Selection::discover(home.path(), &candidates.candidates).unwrap();
        assert!(selection.available());
        assert!(
            !selection.inventory_incomplete(),
            "{}",
            serde_json::to_string(&selection.inventory_report()).unwrap()
        );
        let report = selection.inventory_report();
        assert_eq!(report.schema, crate::browser_inventory::SCHEMA);
        assert_eq!(report.profiles.len(), 7);
        for browser in [
            crate::browser_inventory::BrowserKind::Chrome,
            crate::browser_inventory::BrowserKind::Chromium,
            crate::browser_inventory::BrowserKind::UngoogledChromium,
            crate::browser_inventory::BrowserKind::Firefox,
            crate::browser_inventory::BrowserKind::LibreWolf,
        ] {
            assert!(
                report
                    .profiles
                    .iter()
                    .any(|profile| profile.browser == browser)
            );
        }
        assert!(report.profiles.iter().any(|profile| profile.browser
            == crate::browser_inventory::BrowserKind::Chrome
            && profile.installation == crate::browser_inventory::Installation::Flatpak));
        assert!(report.profiles.iter().any(|profile| profile.browser
            == crate::browser_inventory::BrowserKind::LibreWolf
            && profile.installation == crate::browser_inventory::Installation::Flatpak));
        for profile in &report.profiles {
            assert_eq!(
                profile.bookmarks,
                crate::browser_inventory::Measure::Exact(1)
            );
            assert_eq!(profile.history, crate::browser_inventory::Measure::Exact(1));
            assert_eq!(
                profile.capabilities.bookmarks,
                crate::browser_inventory::Capability::Supported
            );
            assert_eq!(
                profile.capabilities.history,
                crate::browser_inventory::Capability::Supported
            );
            assert_eq!(
                profile.capabilities.passwords,
                crate::browser_inventory::Capability::Unsupported("browser-passwords-not-exported")
            );
            assert_eq!(
                profile.capabilities.website_data,
                crate::browser_inventory::Capability::InventoryOnly
            );
            assert_eq!(
                profile.websites.accounts,
                crate::browser_inventory::Measure::Exact(1)
            );
            assert!(profile.websites.account_evidence);
            assert_eq!(
                profile.extensions,
                crate::browser_inventory::Measure::Exact(1)
            );
            assert_eq!(
                profile.settings,
                crate::browser_inventory::Measure::Exact(1)
            );
        }
        let chrome_inventory = report
            .profiles
            .iter()
            .find(|profile| {
                profile.browser == crate::browser_inventory::BrowserKind::Chrome
                    && profile.installation == crate::browser_inventory::Installation::Native
            })
            .unwrap();
        assert_eq!(
            chrome_inventory.passwords,
            crate::browser_inventory::Measure::Exact(1)
        );
        assert_eq!(
            chrome_inventory.websites.cookies,
            crate::browser_inventory::Measure::Exact(2)
        );
        assert_eq!(
            chrome_inventory.websites.origins,
            crate::browser_inventory::Measure::Exact(2)
        );
        assert_eq!(chrome_inventory.websites.services.len(), 1);
        assert_eq!(
            chrome_inventory.websites.services[0].cookies,
            crate::browser_inventory::Measure::Exact(1)
        );
        assert!(!chrome_inventory.websites.services[0].account_evidence);
        let firefox_inventory = report
            .profiles
            .iter()
            .find(|profile| profile.browser == crate::browser_inventory::BrowserKind::Firefox)
            .unwrap();
        assert_eq!(
            firefox_inventory.passwords,
            crate::browser_inventory::Measure::Exact(1)
        );
        assert_eq!(
            firefox_inventory.websites.services[0].service,
            browser_continuity::sessions::Service::Discord
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn browser_inventory_report_redacts_private_values_and_preserves_sources() {
        let (home, before) = inventory_fixture().unwrap();
        let candidates = crate::account_sources::discover(&crate::account_sources::SourceRoots {
            home: home.path().into(),
            config: home.path().join(".config"),
        });
        let selection = Selection::discover(home.path(), &candidates.candidates).unwrap();
        let public = serde_json::to_string(&selection.inventory_report()).unwrap();
        assert!(!public.contains("PRIVATE_COOKIE_SENTINEL"));
        assert!(!public.contains("PRIVATE_PASSWORD_SENTINEL"));
        assert!(!public.contains("person@example.com"));
        assert!(!public.contains(home.path().to_str().unwrap()));
        for (path, bytes, mode) in before {
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert_eq!(fs::metadata(path).unwrap().permissions().mode(), mode);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn profile_and_domain_exclusions_reach_the_authenticated_plan_and_capture() {
        let home = tempfile::tempdir().unwrap();
        let chrome = home.path().join(".config/chromium/Default");
        chromium_profile(&chrome, "Bookmark", "History", &["example.com"]).unwrap();
        let firefox = firefox_root(home.path(), ".mozilla/firefox", "first.default").unwrap();
        firefox_profile(&firefox, "Bookmark", "History", "example.com").unwrap();
        let candidates = crate::account_sources::discover(&crate::account_sources::SourceRoots {
            home: home.path().into(),
            config: home.path().join(".config"),
        });
        let mut selection = Selection::discover(home.path(), &candidates.candidates).unwrap();
        assert_eq!(selection.selected().profiles().len(), 2);
        let firefox_id = selection
            .inventory()
            .iter()
            .find(|profile| profile.browser == crate::browser_inventory::BrowserKind::Firefox)
            .unwrap()
            .id
            .clone();
        selection.set_profile(&firefox_id, false).unwrap();
        let plan = selection.selected().plan("setup".into(), [7; 32]).unwrap();
        assert_eq!(plan.profiles.len(), 1);
        assert_ne!(plan.profiles[0].id, firefox_id);
        assert!(
            !selection
                .inventory_report()
                .profiles
                .iter()
                .any(|profile| profile.id == firefox_id && profile.included)
        );
        selection.set(Domain::History, false);
        let plan = selection.selected().plan("setup".into(), [7; 32]).unwrap();
        assert_eq!(plan.profiles.len(), 1);
        assert!(plan.profiles[0].bookmarks);
        assert!(!plan.profiles[0].history);
        assert_eq!(plan.profiles[0].allowed_files(), ["Bookmarks"]);
        let selected_sources = selection.selected();
        let (source, selected) = &selected_sources.profiles()[0];
        let captured = source.capture_files(selected).unwrap();
        assert_eq!(
            captured
                .files()
                .iter()
                .map(crate::browser_selection::CapturedFile::name)
                .collect::<Vec<_>>(),
            ["Bookmarks"]
        );
        selection.set(Domain::Bookmarks, false);
        assert!(selection.selected().profiles().is_empty());
        assert!(!fs::read(&firefox.join("places.sqlite")).unwrap().is_empty());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn per_profile_domain_choices_stay_independent_and_exclusion_clears_only_its_profile() {
        let home = tempfile::tempdir().unwrap();
        let chrome = home.path().join(".config/chromium/Default");
        chromium_profile(&chrome, "Bookmark", "History", &["example.com"]).unwrap();
        let firefox = firefox_root(home.path(), ".mozilla/firefox", "first.default").unwrap();
        firefox_profile(&firefox, "Bookmark", "History", "example.com").unwrap();
        let candidates = crate::account_sources::discover(&crate::account_sources::SourceRoots {
            home: home.path().into(),
            config: home.path().join(".config"),
        });
        let mut selection = Selection::discover(home.path(), &candidates.candidates).unwrap();
        let chrome_id = selection
            .inventory()
            .iter()
            .find(|profile| profile.browser == crate::browser_inventory::BrowserKind::Chromium)
            .unwrap()
            .id
            .clone();
        let firefox_id = selection
            .inventory()
            .iter()
            .find(|profile| profile.browser == crate::browser_inventory::BrowserKind::Firefox)
            .unwrap()
            .id
            .clone();

        assert!(
            selection
                .set_profile_domain(&chrome_id, Domain::History, false)
                .unwrap()
        );
        let chrome_state = selection.profile_state(&chrome_id).unwrap();
        let firefox_state = selection.profile_state(&firefox_id).unwrap();
        assert!(chrome_state.included && chrome_state.bookmarks && !chrome_state.history);
        assert!(firefox_state.included && firefox_state.bookmarks && firefox_state.history);
        let plan = selection.selected().plan("setup".into(), [7; 32]).unwrap();
        assert_eq!(plan.profiles.len(), 2);
        let chrome_plan = plan
            .profiles
            .iter()
            .find(|profile| profile.id == chrome_id)
            .unwrap();
        assert!(chrome_plan.bookmarks && !chrome_plan.history);
        let report = selection.inventory_report();
        let chrome_report = report
            .profiles
            .iter()
            .find(|profile| profile.id == chrome_id)
            .unwrap();
        assert!(chrome_report.included);
        assert!(chrome_report.bookmarks_selected);
        assert!(!chrome_report.history_selected);
        let firefox_report = report
            .profiles
            .iter()
            .find(|profile| profile.id == firefox_id)
            .unwrap();
        assert!(firefox_report.bookmarks_selected && firefox_report.history_selected);

        // Excluding a profile clears every domain it owns and leaves the other
        // profile exactly as it was.
        selection.set_profile(&firefox_id, false).unwrap();
        let excluded = selection.profile_state(&firefox_id).unwrap();
        assert!(!excluded.included && !excluded.bookmarks && !excluded.history);
        assert_eq!(selection.selected().profiles().len(), 1);
        let plan = selection.selected().plan("setup".into(), [7; 32]).unwrap();
        assert_eq!(plan.profiles.len(), 1);
        assert_eq!(plan.profiles[0].id, chrome_id);
        assert!(plan.profiles[0].bookmarks && !plan.profiles[0].history);
        assert!(
            !selection
                .inventory_report()
                .profiles
                .iter()
                .find(|profile| profile.id == firefox_id)
                .unwrap()
                .included
        );

        // Re-including restores the domains its reviewed source supports, and
        // the global control still changes both profiles at once.
        selection.set_profile(&firefox_id, true).unwrap();
        let restored = selection.profile_state(&firefox_id).unwrap();
        assert!(restored.included && restored.bookmarks && restored.history);
        selection.set(Domain::History, false);
        let plan = selection.selected().plan("setup".into(), [7; 32]).unwrap();
        assert_eq!(plan.profiles.len(), 2);
        assert!(plan.profiles.iter().all(|profile| profile.bookmarks));
        assert!(plan.profiles.iter().all(|profile| !profile.history));
        assert!(!selection.chosen(Domain::History));
        assert!(selection.chosen(Domain::Bookmarks));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn website_only_profile_is_visible_but_never_becomes_an_empty_plan_entry() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".config/chromium/Default");
        fs::create_dir_all(directory.join("Network")).unwrap();
        let cookies = rusqlite::Connection::open(directory.join("Network/Cookies")).unwrap();
        cookies
            .execute_batch(
                "CREATE TABLE cookies(host_key TEXT, value TEXT);
                 INSERT INTO cookies VALUES('example.com','PRIVATE_COOKIE_SENTINEL');
                 INSERT INTO cookies VALUES('other.example','PRIVATE_COOKIE_SENTINEL');",
            )
            .unwrap();
        drop(cookies);
        let candidates = crate::account_sources::discover(&crate::account_sources::SourceRoots {
            home: home.path().into(),
            config: home.path().join(".config"),
        });
        let mut selection = Selection::discover(home.path(), &candidates.candidates).unwrap();
        let profile = selection.inventory().first().unwrap();
        let profile_id = profile.id.clone();
        assert!(!profile.exportable);
        assert!(!profile.included);
        assert_eq!(
            profile.capabilities.bookmarks,
            crate::browser_inventory::Capability::Absent
        );
        assert_eq!(
            profile.websites.cookies,
            crate::browser_inventory::Measure::Exact(2)
        );
        assert_eq!(
            profile.websites.origins,
            crate::browser_inventory::Measure::Exact(2)
        );
        assert!(selection.selected().profiles().is_empty());
        assert_eq!(
            selection.set_profile(&profile_id, true),
            Err("installer-browser-source-not-exportable")
        );
        assert!(
            selection
                .inventory_report()
                .profiles
                .iter()
                .any(|profile| !profile.exportable && !profile.included)
        );
    }

    #[test]
    fn reviewed_selection_refuses_duplicates_widening_and_changed_identity() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join(".config/chromium/Default");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Bookmarks"), b"bookmarks").unwrap();
        let inventory = crate::account_sources::discover(&crate::account_sources::SourceRoots {
            home: home.path().into(),
            config: home.path().join(".config"),
        });
        let selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        let selected = selection.selected();
        let (source, profile) = &selected.profiles()[0];
        assert!(
            SelectedSources::from_reviewed(vec![
                (Arc::clone(source), profile.clone()),
                (Arc::clone(source), profile.clone()),
            ])
            .is_err()
        );
        let mut widened = profile.clone();
        widened.history = true;
        assert!(SelectedSources::from_reviewed(vec![(Arc::clone(source), widened),]).is_err());
        let mut changed = profile.clone();
        changed.relative_path.push("other".into());
        assert!(SelectedSources::from_reviewed(vec![(Arc::clone(source), changed),]).is_err());
        fs::rename(&directory, directory.with_file_name("old")).unwrap();
        fs::create_dir(&directory).unwrap();
        assert!(source.capture_files(profile).is_err());
    }

    #[test]
    fn browsing_domain_selection_captures_only_its_actual_reviewed_files() {
        let home = tempfile::tempdir().unwrap();
        let profile = home.path().join(".config/chromium/Default");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("Bookmarks"), b"bookmarks").unwrap();
        fs::write(profile.join("History"), b"private history").unwrap();
        fs::write(profile.join("Cookies"), b"not consented").unwrap();
        let inventory = crate::account_sources::discover(&crate::account_sources::SourceRoots {
            home: home.path().into(),
            config: home.path().join(".config"),
        });
        let mut selection = Selection::discover(home.path(), &inventory.candidates).unwrap();
        assert!(selection.available());
        assert!(selection.chosen(Domain::Bookmarks));
        assert!(selection.chosen(Domain::History));
        selection.set(Domain::History, false);
        let selected = selection.selected();
        let plan = selected.plan("exact-setup".into(), [3; 32]).unwrap();
        assert_eq!(plan.profiles.len(), 1);
        assert!(!plan.profiles[0].history);
        assert_eq!(plan.profiles[0].allowed_files(), ["Bookmarks"]);
        let (source, profile) = &selected.profiles()[0];
        let captured = source.capture_files(profile).unwrap();
        assert_eq!(captured.files().len(), 1);
        assert_eq!(captured.files()[0].name(), "Bookmarks");
        assert_eq!(captured.files()[0].bytes(), b"bookmarks");
        selection.set(Domain::Bookmarks, false);
        assert!(selection.selected().profiles().is_empty());
        assert_eq!(
            fs::read(home.path().join(".config/chromium/Default/History")).unwrap(),
            b"private history"
        );
        assert_eq!(
            fs::read(home.path().join(".config/chromium/Default/Cookies")).unwrap(),
            b"not consented"
        );
    }
}
