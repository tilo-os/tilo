//! Exact legacy browser profiles discovered inside an already granted home.
//! Metadata discovery never reads browser databases, cookies or credentials.
use std::io::{self, Read};
use std::path::{Component, Path};
use std::sync::Arc;

use browser_continuity::{Profile, Provider};
use tilo_files::native::atomic::{Privacy, StorageDirectory};

use super::account_sources::{Candidate, Origin, Provider as AccountProvider};
use super::browser_selection::pinned::PinnedDirectory;
use super::nofollow::metadata_nofollow;

const MAX_PROFILE_DIRECTORIES: usize = 128;
const MAX_INI: u64 = 64 * 1024;

pub struct ReviewedProfile {
    pub profile: Profile,
    pub label: String,
    directory: Arc<PinnedDirectory>,
}
impl ReviewedProfile {
    /// Revalidate the reviewed profile and its retained directory namespace.
    ///
    /// # Errors
    /// Refuses invalid profile metadata, replaced directories or unsafe namespace control.
    pub fn validate(&self) -> io::Result<()> {
        self.profile.validate().map_err(invalid)?;
        self.directory.validate()?;
        storage(&self.directory)?.validate_namespace_control()
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
        match candidate.provider {
            AccountProvider::Chromium => {
                chromium(&directory, &relative, &candidate.label, &mut profiles)?
            }
            AccountProvider::Firefox => {
                firefox(&directory, &relative, &candidate.label, &mut profiles)?
            }
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
fn add(
    directory: Arc<PinnedDirectory>,
    relative: Vec<String>,
    label: String,
    provider: Provider,
    selected: (bool, bool),
    output: &mut Vec<ReviewedProfile>,
) -> io::Result<()> {
    let identity = serde_json::to_vec(&(provider, &relative)).map_err(invalid)?;
    let profile = Profile::new(
        blake3::hash(&identity).to_hex().to_string(),
        provider,
        relative,
        selected.0,
        selected.1,
    )
    .map_err(invalid)?;
    if output.iter().any(|item| item.profile.id == profile.id) {
        return Ok(());
    }
    output.push(ReviewedProfile {
        profile,
        label,
        directory,
    });
    Ok(())
}
fn chromium(
    root: &Arc<PinnedDirectory>,
    relative: &[String],
    label: &str,
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
        if selected == (false, false) {
            continue;
        }
        let mut path = relative.to_vec();
        path.push(name.clone());
        add(
            directory,
            path,
            format!("{label} · {name}"),
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
        if !regular(&directory, "places.sqlite")? {
            continue;
        }
        let mut path = relative.to_vec();
        path.extend(names);
        add(
            directory,
            path,
            format!("{label} · {name}"),
            Provider::Firefox,
            (true, true),
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
