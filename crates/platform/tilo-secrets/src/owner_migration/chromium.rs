//! The existing Linux OSCrypt password for one explicitly selected owner profile.
use crate::{Secret, SecretError};
use std::{fs::File, os::unix::fs::MetadataExt};

/// Upstream build brands with verified fixed libsecret application attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromiumBrand {
    Chrome,
    Chromium,
    /// Public Slack 4.52.178 / Electron 44.4.5 OSCrypt contract; callers must
    /// authenticate its exact public application before selected capture.
    Slack452178,
}
impl ChromiumBrand {
    pub(crate) fn application(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Chromium => "chromium",
            Self::Slack452178 => "Slack",
        }
    }
}

/// Native consent capability. It retains the selected directory, not a wildcard
/// keychain query. It cannot be serialized into an action or copied into CRDT state.
pub struct ChromiumKey {
    profile: File,
    identity: (u32, u64, u64),
    brand: ChromiumBrand,
}
impl ChromiumKey {
    /// Call only after explicit consent to include this browser's selected sessions.
    ///
    /// # Errors
    /// Refuses a non-directory, foreign owner, or elevated caller.
    pub fn new(profile: &File, brand: ChromiumBrand) -> Result<Self, SecretError> {
        let facts = profile.metadata().map_err(|_| SecretError::Denied)?;
        let owner = unsafe { libc::getuid() };
        if !facts.is_dir()
            || facts.uid() != owner
            || facts.mode() & 0o022 != 0
            || owner == 0
            || unsafe { libc::geteuid() } != owner
        {
            return Err(SecretError::Denied);
        }
        Ok(Self {
            profile: profile.try_clone().map_err(|_| SecretError::Denied)?,
            identity: (owner, facts.dev(), facts.ino()),
            brand,
        })
    }
    /// Existing password only: no item, collection, password, or fallback is created.
    /// The owner's Secret Service may display its own unlock prompt.
    ///
    /// # Errors
    /// Refuses changed consent custody, a foreign bus/service, ambiguous matches,
    /// locked/denied custody, and unavailable or malformed passwords.
    pub fn read_blocking(&self) -> Result<Option<Secret>, SecretError> {
        let facts = self.profile.metadata().map_err(|_| SecretError::Denied)?;
        if !facts.is_dir()
            || (facts.uid(), facts.dev(), facts.ino()) != self.identity
            || facts.mode() & 0o022 != 0
            || unsafe { libc::getuid() } != self.identity.0
            || unsafe { libc::geteuid() } != self.identity.0
        {
            return Err(SecretError::Denied);
        }
        crate::linux::read_chromium(self.identity.0, self.brand.application())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_scope_refuses_files_and_elevation_without_contacting_a_keyring() {
        let directory = tempfile::tempdir().unwrap();
        let file = File::create(directory.path().join("not-profile")).unwrap();
        assert!(ChromiumKey::new(&file, ChromiumBrand::Chrome).is_err());
        if unsafe { libc::getuid() } == 0 {
            assert!(
                ChromiumKey::new(
                    &File::open(directory.path()).unwrap(),
                    ChromiumBrand::Chrome
                )
                .is_err()
            );
        } else {
            assert!(
                ChromiumKey::new(
                    &File::open(directory.path()).unwrap(),
                    ChromiumBrand::Chromium
                )
                .is_ok()
            );
        }
        assert_eq!(ChromiumBrand::Chrome.application(), "chrome");
        assert_eq!(ChromiumBrand::Chromium.application(), "chromium");
        assert_eq!(ChromiumBrand::Slack452178.application(), "Slack");
    }
}
