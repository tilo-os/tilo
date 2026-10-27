//! What the volume's own prompt asks for, in the person's words.
//!
//! Before anyone has a sign-in on the volume, and after "Can't sign in",
//! systemd-cryptsetup asks for a secret that opens one of the volume's key
//! slots, and the boot screen says which. The installer marks the slot its
//! startup secret went into with a `tilo-startup-secret` token when that
//! secret is the account password, so the screen can say "password" instead
//! of "disk passphrase"; it offers the recovery key only where a
//! `systemd-recovery` token has a slot.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

/// The secret a person can type at the volume's prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Secret {
    /// The account password, which the installer also set as the disk's.
    Password,
    /// A disk passphrase of its own.
    Passphrase,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ways {
    pub secret: Option<Secret>,
    pub recovery: bool,
}

impl Ways {
    /// The prompt the theme shows (`tilo-unlock-ask:<kind>`), or `None` when
    /// nothing the person types opens the volume.
    pub fn kind(self) -> Option<&'static str> {
        match (self.secret, self.recovery) {
            (Some(Secret::Password), true) => Some("password-recovery"),
            (Some(Secret::Passphrase), true) => Some("passphrase-recovery"),
            (Some(Secret::Password), false) => Some("password"),
            (Some(Secret::Passphrase), false) => Some("passphrase"),
            (None, true) => Some("recovery"),
            (None, false) => None,
        }
    }
}

#[derive(Deserialize)]
struct Metadata {
    #[serde(default)]
    keyslots: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    tokens: std::collections::BTreeMap<String, Token>,
}

#[derive(Deserialize)]
struct Token {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    keyslots: Vec<String>,
}

/// The ways in that `cryptsetup luksDump --dump-json-metadata` shows.
///
/// A slot no token claims is a passphrase slot. Slots that tokens claim are
/// opened by what the token names (the recovery key, a person's sign-in, a
/// security key, the TPM), except the slot of the installer's
/// `tilo-startup-secret` token, which is the account password whether it was
/// enrolled as a passphrase or as the TPM's PIN.
pub fn ways(metadata: &str) -> Ways {
    let Ok(metadata) = serde_json::from_str::<Metadata>(metadata) else {
        return Ways::default();
    };
    let live = |token: &Token| {
        token
            .keyslots
            .iter()
            .any(|slot| metadata.keyslots.contains_key(slot))
    };
    let of = |kind: &str| {
        metadata
            .tokens
            .values()
            .any(|token| token.kind == kind && live(token))
    };
    let claimed: Vec<&String> = metadata
        .tokens
        .values()
        .flat_map(|token| token.keyslots.iter())
        .collect();
    let plain = metadata
        .keyslots
        .keys()
        .any(|slot| !claimed.contains(&slot));
    let secret = if of("tilo-startup-secret") {
        Some(Secret::Password)
    } else if plain {
        Some(Secret::Passphrase)
    } else {
        None
    };
    Ways {
        secret,
        recovery: of("systemd-recovery"),
    }
}

/// The volume header's metadata, through `cryptsetup`.
pub fn read(cryptsetup: &Path, device: &Path) -> Option<String> {
    let output = Command::new(cryptsetup)
        .arg("luksDump")
        .arg("--dump-json-metadata")
        .arg(device)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOT: &str = r#"{"type":"luks2"}"#;

    fn metadata(slots: &[&str], tokens: &[(&str, &[&str])]) -> String {
        let slots: Vec<String> = slots.iter().map(|id| format!(r#""{id}":{SLOT}"#)).collect();
        let tokens: Vec<String> = tokens
            .iter()
            .enumerate()
            .map(|(id, (kind, slots))| {
                let slots: Vec<String> = slots.iter().map(|s| format!(r#""{s}""#)).collect();
                format!(
                    r#""{id}":{{"type":"{kind}","keyslots":[{}]}}"#,
                    slots.join(",")
                )
            })
            .collect();
        format!(
            r#"{{"keyslots":{{{}}},"tokens":{{{}}},"segments":{{}}}}"#,
            slots.join(","),
            tokens.join(",")
        )
    }

    #[test]
    fn the_header_comes_from_cryptsetup() {
        use std::os::unix::fs::PermissionsExt as _;
        let _serial = crate::test_support::serial();
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("cryptsetup");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\n[ \"$*\" = 'luksDump --dump-json-metadata /dev/vda3' ] || exit 1\n\
                 printf '%s' '{}'\n",
                metadata(&["1", "2"], &[("systemd-recovery", &["1"])])
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let header = read(&program, Path::new("/dev/vda3")).expect("the header");
        assert_eq!(ways(&header).kind(), Some("passphrase-recovery"));
        assert_eq!(read(&program, Path::new("/dev/other")), None);
    }

    #[test]
    fn the_installers_one_secret_reads_as_the_password() {
        // As the installer leaves it: the recovery key, and the account
        // password marked as the startup secret.
        let ways = ways(&metadata(
            &["1", "2"],
            &[
                ("systemd-recovery", &["1"]),
                ("tilo-startup-secret", &["2"]),
            ],
        ));
        assert_eq!(ways.kind(), Some("password-recovery"));
    }

    #[test]
    fn a_separate_disk_secret_reads_as_the_disk_passphrase() {
        let ways = ways(&metadata(&["1", "2"], &[("systemd-recovery", &["1"])]));
        assert_eq!(ways.kind(), Some("passphrase-recovery"));
    }

    #[test]
    fn the_recovery_key_is_offered_only_where_it_has_a_slot() {
        assert_eq!(ways(&metadata(&["0"], &[])).kind(), Some("passphrase"));
        let removed = ways(&metadata(&["2"], &[("systemd-recovery", &["1"])]));
        assert_eq!(removed.kind(), Some("passphrase"));
    }

    #[test]
    fn slots_other_tokens_open_are_not_typed() {
        // A person's sign-in slot and a security key: only the recovery key
        // is typed at the volume's prompt.
        let ways = ways(&metadata(
            &["0", "1", "3"],
            &[
                ("tilo-signin", &["0"]),
                ("systemd-recovery", &["1"]),
                ("systemd-fido2", &["3"]),
            ],
        ));
        assert_eq!(ways.kind(), Some("recovery"));
        assert_eq!(super::ways("not json").kind(), None);
    }

    #[test]
    fn a_password_token_without_its_slot_offers_no_password() {
        // The startup secret's slot was removed after migration; the token
        // that named it no longer claims anything.
        let ways = ways(&metadata(
            &["0", "1"],
            &[
                ("tilo-signin", &["0"]),
                ("systemd-recovery", &["1"]),
                ("tilo-startup-secret", &[]),
            ],
        ));
        assert_eq!(ways.kind(), Some("recovery"));
    }
}
