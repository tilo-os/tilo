//! Cold-boot scenarios: boot a medium in a throwaway VM and drive it end to
//! end through the same published actions an agent or a screen reader uses.
//!
//! `tilo-vm scenario install` boots an installer medium with `os/dev/vm.sh`
//! (the headless QEMU path the release gate uses), reaches the installer
//! over its console channel, installs, powers off through the installer's
//! own Finish action, cold-boots the target, unlocks it at the only door
//! early boot has (the keyboard), and checks that first boot reached the
//! greeter running the tree the installer wrote. Every phase is timed into
//! the receipt; no artifact may carry the passphrase or a recovery key.

use std::io::Read as _;
use std::path::{Path, PathBuf};

pub mod console;
pub mod drive;
pub mod fixture;

/// How the target disk is laid out before the install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    /// A blank disk, erased and used whole.
    Erase,
    /// Existing systems plus a prepared empty Linux root and a 2 GiB EFI
    /// partition; only the prepared root is formatted.
    PreparedAlongside,
    /// Existing systems plus unallocated space; the installer adds its own
    /// EFI partition and root there and touches nothing else.
    GuidedAlongside,
}

impl Layout {
    /// The installer's `set-method` choice.
    pub fn method(self) -> &'static str {
        match self {
            Self::Erase => "erase",
            Self::PreparedAlongside | Self::GuidedAlongside => "alongside",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Erase => "erase",
            Self::PreparedAlongside => "prepared-alongside",
            Self::GuidedAlongside => "guided-alongside",
        }
    }
}

/// How the always-on disk encryption unlocks at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Unlock {
    /// A strong passphrase, no TPM (the default).
    Passphrase,
    /// A short PIN sealed to a software TPM. The installer offers this only
    /// when the target's verified-boot chain checks out, which needs a
    /// firmware store that trusts the image (`TILO_SECBOOT_KEYS`).
    TpmPin,
}

impl Unlock {
    pub fn name(self) -> &'static str {
        match self {
            Self::Passphrase => "passphrase",
            Self::TpmPin => "tpm-pin",
        }
    }

    /// The disposable secret a scenario target is installed with. Fixed and
    /// public on purpose: it protects nothing, and a fixed value is what the
    /// artifact scrubber searches for.
    pub fn scenario_secret(self) -> Secret {
        Secret::new(match self {
            Self::Passphrase => "scenario-passphrase-2026",
            Self::TpmPin => "804927",
        })
    }
}

/// Graphics for both boots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Graphics {
    /// virgl through egl-headless (what the installed desktop needs).
    Gl,
    /// Plain virtio VGA; the installer and greeter render in software.
    Software,
}

impl Graphics {
    /// `vm.sh`'s display flag.
    pub fn flag(self) -> &'static str {
        match self {
            Self::Gl => "--gl",
            Self::Software => "--2d",
        }
    }
}

/// A secret the scenario itself chose: never `Debug`-printed, wiped when
/// dropped, and only ever sent to the guest (console argument or typed at
/// the unlock prompt through stdin, never argv).
pub struct Secret(zeroize::Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(zeroize::Zeroizing::new(value.into()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// Is `text` shaped like a recovery key: at least six dash-separated groups
/// of four to eight letters and digits? A UUID has five groups.
pub fn has_recovery_key_shape(text: &str) -> bool {
    text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
        .any(|token| {
            let token = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
            let groups: Vec<&str> = token.split('-').collect();
            groups.len() >= 6
                && groups.iter().all(|group| {
                    (4..=8).contains(&group.len())
                        && group.chars().all(|c| c.is_ascii_alphanumeric())
                })
        })
}

/// The installer backend's private marker for the one-time recovery key.
pub const RECOVERY_KEY_MARKER: &str = "::tilo-recovery-key::";

/// Empty every file under `dir` that contains `secret`, the recovery-key
/// marker, or anything shaped like a recovery key, and return the files it
/// emptied. The same guarantee the release cycle gives its uploads: a
/// diagnostic that could carry recovery material is withheld, never kept.
///
/// # Errors
///
/// An I/O error walking or rewriting the directory.
pub fn scrub(dir: &Path, secret: &str) -> std::io::Result<Vec<PathBuf>> {
    let mut withheld = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let mut bytes = Vec::new();
            std::fs::File::open(entry.path())?.read_to_end(&mut bytes)?;
            let text = String::from_utf8_lossy(&bytes);
            if (!secret.is_empty() && text.contains(secret))
                || text.contains(RECOVERY_KEY_MARKER)
                || has_recovery_key_shape(&text)
            {
                std::fs::write(entry.path(), b"")?;
                withheld.push(entry.path());
            }
        }
    }
    withheld.sort();
    Ok(withheld)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_key_shapes_are_recognised_and_uuids_are_not() {
        assert!(has_recovery_key_shape(
            "key bbbbbbbb-cccccccc-dddddddd-ffffffff-gggggggg-hhhhhhhh-jjjjjjjj-kkkkkkkk."
        ));
        assert!(!has_recovery_key_shape(
            "00000000-0000-4000-8000-000000000003"
        ));
        assert!(!has_recovery_key_shape(
            "installer: Copying verified Tilo system files…"
        ));
    }

    #[test]
    fn the_scrubber_withholds_secret_bearing_files_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        let clean = dir.path().join("state.json");
        let leaked = dir.path().join("nested/serial.log");
        let marker = dir.path().join("marker.txt");
        let key = dir.path().join("key.txt");
        std::fs::write(
            &clean,
            r#"{"step":"install","recovery_key":{"available":true}}"#,
        )
        .unwrap();
        std::fs::write(&leaked, "typed scenario-passphrase-2026 at the prompt").unwrap();
        std::fs::write(&marker, "::tilo-recovery-key::artifact-canary").unwrap();
        std::fs::write(
            &key,
            "bbbbbbbb-cccccccc-dddddddd-ffffffff-gggggggg-hhhhhhhh-jjjjjjjj-kkkkkkkk",
        )
        .unwrap();
        let withheld = scrub(dir.path(), "scenario-passphrase-2026").unwrap();
        assert_eq!(withheld.len(), 3, "{withheld:?}");
        for path in [&leaked, &marker, &key] {
            assert!(
                std::fs::read(path).unwrap().is_empty(),
                "{}",
                path.display()
            );
        }
        assert!(!std::fs::read(&clean).unwrap().is_empty());
    }

    #[test]
    fn secrets_never_debug_print() {
        let secret = Unlock::Passphrase.scenario_secret();
        assert_eq!(format!("{secret:?}"), "Secret([redacted])");
        assert_eq!(Layout::GuidedAlongside.method(), "alongside");
        assert_eq!(Graphics::Software.flag(), "--2d");
    }
}
