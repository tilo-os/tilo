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
    #[cfg(any(test, feature = "test-support"))]
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
pub struct Selection {
    profiles: Vec<Arc<ReviewedProfile>>,
    bookmarks: bool,
    history: bool,
}
impl Default for Selection {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
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
        Ok(Self {
            profiles: browser_profiles::discover(home, candidates)?
                .into_iter()
                .map(Arc::new)
                .collect(),
            ..Self::default()
        })
    }
    pub fn available(&self) -> bool {
        !self.profiles.is_empty()
    }
    pub fn chosen(&self, domain: Domain) -> bool {
        match domain {
            Domain::Bookmarks => self.bookmarks,
            Domain::History => self.history,
        }
    }
    pub fn set(&mut self, domain: Domain, included: bool) {
        match domain {
            Domain::Bookmarks => self.bookmarks = included,
            Domain::History => self.history = included,
        }
    }
    pub fn selected(&self) -> SelectedSources {
        SelectedSources {
            profiles: self
                .profiles
                .iter()
                .filter_map(|source| {
                    let mut profile = source.profile.clone();
                    profile.bookmarks &= self.bookmarks;
                    profile.history &= self.history;
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
