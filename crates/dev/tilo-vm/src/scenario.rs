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
use zeroize::Zeroize as _;

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

/// A disposable or supplied secret: never `Debug`-printed, wiped when
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

/// Credentials supplied over stdin for a retained install. The default
/// disposable scenario uses one public secret and leaves `disk` unset.
pub struct Credentials {
    pub account: Secret,
    pub disk: Option<Secret>,
}

impl Credentials {
    pub fn disposable(unlock: Unlock) -> Self {
        Self {
            account: unlock.scenario_secret(),
            disk: None,
        }
    }

    pub fn unlock_secret(&self) -> &Secret {
        self.disk.as_ref().unwrap_or(&self.account)
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialsInput {
    account_password: String,
    disk_passphrase: String,
}

impl Drop for CredentialsInput {
    fn drop(&mut self) {
        self.account_password.zeroize();
        self.disk_passphrase.zeroize();
    }
}

/// Parse one bounded, exact JSON object without ever echoing supplied text.
///
/// # Errors
///
/// Invalid, oversized, non-alphanumeric or matching credentials.
pub fn parse_credentials(bytes: &[u8]) -> Result<Credentials, &'static str> {
    if bytes.is_empty() || bytes.len() > 8192 {
        return Err("credentials stdin must contain one bounded JSON object");
    }
    let mut input: CredentialsInput = serde_json::from_slice(bytes)
        .map_err(|_| "credentials stdin must contain account_password and disk_passphrase")?;
    let valid = |value: &str| {
        !value.is_empty()
            && value.len() <= 4096
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    };
    if !valid(&input.account_password)
        || !valid(&input.disk_passphrase)
        || input.account_password == input.disk_passphrase
    {
        return Err("account and disk secrets must be distinct lowercase ASCII letters and digits");
    }
    Ok(Credentials {
        account: Secret::new(std::mem::take(&mut input.account_password)),
        disk: Some(Secret::new(std::mem::take(&mut input.disk_passphrase))),
    })
}

/// Read credentials before creating artifacts or booting the VM.
///
/// # Errors
///
/// Standard input could not be read or did not contain valid credentials.
pub fn credentials_from_stdin() -> Result<Credentials, &'static str> {
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    std::io::stdin()
        .take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| "could not read credentials stdin")?;
    parse_credentials(&bytes)
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
pub fn scrub(dir: &Path, secrets: &[&str]) -> std::io::Result<Vec<PathBuf>> {
    scrub_with(dir, secrets, |_| true)
}

/// Keep large disk payloads outside the log walk. A log that exceeds the scan
/// bound is withheld rather than loaded into memory or retained unchecked.
pub(crate) fn scrub_with(
    dir: &Path,
    secrets: &[&str],
    enter_directory: impl Fn(&Path) -> bool,
) -> std::io::Result<Vec<PathBuf>> {
    const MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;
    let mut withheld = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if enter_directory(&entry.path()) {
                    pending.push(entry.path());
                }
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            if entry.metadata()?.len() > MAX_LOG_BYTES {
                std::fs::write(entry.path(), b"")?;
                withheld.push(entry.path());
                continue;
            }
            let mut bytes = Vec::new();
            std::fs::File::open(entry.path())?.read_to_end(&mut bytes)?;
            if contains_sensitive_text(&bytes, secrets) {
                std::fs::write(entry.path(), b"")?;
                withheld.push(entry.path());
            }
        }
    }
    withheld.sort();
    Ok(withheld)
}

fn contains_sensitive_text(bytes: &[u8], secrets: &[&str]) -> bool {
    let sensitive = |text: &str| {
        secrets
            .iter()
            .any(|secret| !secret.is_empty() && text.contains(secret))
            || text.contains(RECOVERY_KEY_MARKER)
            || has_recovery_key_shape(text)
    };
    if sensitive(&String::from_utf8_lossy(bytes)) {
        return true;
    }
    if !bytes.contains(&0) {
        return false;
    }
    for little_endian in [true, false] {
        let units = bytes
            .chunks_exact(2)
            .map(|pair| {
                let first = pair.first().copied().unwrap_or_default();
                let second = pair.get(1).copied().unwrap_or_default();
                if little_endian {
                    u16::from_le_bytes([first, second])
                } else {
                    u16::from_be_bytes([first, second])
                }
            })
            .collect::<Vec<_>>();
        if sensitive(&String::from_utf16_lossy(&units)) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recovery key shapes are recognised and UUIDs are not; the scrubber withholds secret-bearing files
    /// and keeps the rest, checks both retained install secrets, and withholds UTF-16 runtime log secrets.
    #[test]
    fn the_scrubber_withholds_every_secret_shape_it_retains() {
        // Phase: recovery_key_shapes_are_recognised_and_uuids_are_not.
        {
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

        // Phase: the_scrubber_withholds_secret_bearing_files_and_keeps_the_rest.
        {
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
            let withheld = scrub(dir.path(), &["scenario-passphrase-2026"]).unwrap();
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

        // Phase: scrubber_checks_both_retained_install_secrets.
        {
            let dir = tempfile::tempdir().unwrap();
            let account = dir.path().join("account.txt");
            let disk = dir.path().join("disk.txt");
            std::fs::write(&account, "account-random-2026").unwrap();
            std::fs::write(&disk, "disk-random-unlock-2026").unwrap();
            let withheld = scrub(
                dir.path(),
                &["account-random-2026", "disk-random-unlock-2026"],
            )
            .unwrap();
            assert_eq!(withheld.len(), 2);
            assert!(std::fs::read(account).unwrap().is_empty());
            assert!(std::fs::read(disk).unwrap().is_empty());
        }

        // Phase: scrubber_withholds_utf16_runtime_log_secrets.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("uart.log");
            let utf16 = "typed disksecret456"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>();
            std::fs::write(&path, utf16).unwrap();
            let withheld = scrub(dir.path(), &["disksecret456"]).unwrap();
            assert_eq!(withheld, std::slice::from_ref(&path));
            assert!(std::fs::read(path).unwrap().is_empty());
        }

        // Phase: oversized_logs_are_withheld_without_loading_them_into_memory.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("serial.log");
            std::fs::File::create(&path)
                .unwrap()
                .set_len(64 * 1024 * 1024 + 1)
                .unwrap();
            assert_eq!(scrub(dir.path(), &[]).unwrap(), [path.clone()]);
            assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
        }
    }

    /// Credentials from stdin are distinct, bounded and never echoed, and secrets never debug-print.
    #[test]
    fn install_credentials_are_bounded_distinct_and_never_printed() {
        // Phase: credentials_from_stdin_are_distinct_bounded_and_never_echoed.
        {
            let account = "accountrandom2026";
            let disk = "diskrandomunlock2026";
            let input = serde_json::json!({"account_password": account, "disk_passphrase": disk});
            let credentials = parse_credentials(input.to_string().as_bytes()).unwrap();
            assert_eq!(credentials.account.expose(), account);
            assert_eq!(credentials.unlock_secret().expose(), disk);
            assert_eq!(format!("{:?}", credentials.account), "Secret([redacted])");
            for bad in [
                serde_json::json!({"account_password": account, "disk_passphrase": account}),
                serde_json::json!({"account_password": "", "disk_passphrase": disk}),
                serde_json::json!({"account_password": account, "disk_passphrase": "line\nother"}),
                serde_json::json!({"account_password": account, "disk_passphrase": "Uppercase"}),
                serde_json::json!({"account_password": "quoted\"account", "disk_passphrase": disk}),
                serde_json::json!({"account_password": account, "disk_passphrase": disk, "extra": true}),
            ] {
                let error = parse_credentials(bad.to_string().as_bytes()).err().unwrap();
                assert!(!error.contains(account) && !error.contains(disk));
            }
        }

        // Phase: secrets_never_debug_print.
        {
            let secret = Unlock::Passphrase.scenario_secret();
            assert_eq!(format!("{secret:?}"), "Secret([redacted])");
            assert_eq!(Layout::GuidedAlongside.method(), "alongside");
            assert_eq!(Graphics::Software.flag(), "--2d");
        }
    }
}
