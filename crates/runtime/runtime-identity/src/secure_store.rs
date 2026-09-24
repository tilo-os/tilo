//! Host-side storage for identity signing material.
//!
//! The preferred backend is the freedesktop Secret Service, reached through
//! the standard `secret-tool` client shipped by libsecret.  A session without
//! a Secret Service (a rescue shell, CI, or a non-GNOME/KDE desktop) falls back
//! to a mode-0600 file below the mode-0700 identity directory.  That fallback
//! is deliberately named *broker file*: the launcher may read it, sandboxed
//! applications never receive either its path or its bytes.
//!
//! `SecretProvider` is the hardware-backed seam. A TPM/FIDO implementation
//! can implement it and pass it to the `_with_provider` APIs; this crate does
//! not claim hardware protection merely because `/dev/tpm*` exists.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use zeroize::Zeroizing;

// Secret Service transports the hex encoding through the bounded subprocess
// helper, so the decoded value may consume at most half its input ceiling.
const MAX_IDENTITY_SECRET_BYTES: u64 = (bounded_command::MAX_INPUT_BYTES / 2) as u64;

use crate::{KeyProtection, SecretReference};

const PURPOSE_ATTRIBUTE: &str = "tilo-identity-signing-v1";

fn validate_software_locator(locator: &str) -> io::Result<()> {
    if locator.len() == 64 && locator.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid identity secret locator",
        ))
    }
}

/// Pluggable storage boundary for private signing shares.
///
/// Implementations must never log `secret`, must authenticate the retrieved
/// value, and must make `store` idempotent for the same locator/value pair.
pub trait SecretProvider {
    fn protection(&self) -> KeyProtection;
    fn backend_id(&self) -> &'static str;

    /// # Errors
    ///
    /// Returns an error if `secret` can't be stored, or if `locator` is
    /// already in use by a different secret.
    fn store(&self, locator: &str, secret: &[u8]) -> io::Result<()>;

    /// # Errors
    ///
    /// Returns an error if no secret is stored at `locator`, or it can't be
    /// authenticated and retrieved.
    fn load(&self, locator: &str) -> io::Result<Zeroizing<Vec<u8>>>;

    /// # Errors
    ///
    /// Returns an error if the secret at `locator` can't be removed.
    fn remove(&self, locator: &str) -> io::Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SecretServiceProvider;

impl SecretProvider for SecretServiceProvider {
    fn protection(&self) -> KeyProtection {
        KeyProtection::SecretService
    }

    fn backend_id(&self) -> &'static str {
        "secret-service-v1"
    }

    fn store(&self, locator: &str, secret: &[u8]) -> io::Result<()> {
        validate_software_locator(locator)?;
        if secret.is_empty() || secret.len() as u64 > MAX_IDENTITY_SECRET_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "identity secret has an invalid length",
            ));
        }
        // secret-tool's stdin is text. Hex is unambiguous, contains no NULs,
        // and is decoded into a zeroizing buffer on lookup.
        let encoded = Zeroizing::new(hex::encode(secret));
        let output = bounded_command::run(
            "secret-tool",
            &[
                "store",
                "--label=Tilo identity signing key",
                "tilo-purpose",
                PURPOSE_ATTRIBUTE,
                "tilo-identity-key",
                locator,
            ],
            Some(encoded.as_bytes()),
            1024,
            Duration::from_secs(5),
        )
        .map_err(io::Error::other)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(
                "Secret Service refused or could not store the identity key",
            ))
        }
    }

    fn load(&self, locator: &str) -> io::Result<Zeroizing<Vec<u8>>> {
        secret_service_load(locator, &[OsStr::new("secret-tool")])
    }

    fn remove(&self, locator: &str) -> io::Result<()> {
        secret_service_remove(locator, &[OsStr::new("secret-tool")])
    }
}

/// Run the Secret Service client named by `argv` (the program, then any
/// arguments that precede the subcommand) with `subcommand` appended.
fn secret_tool(
    argv: &[&OsStr],
    subcommand: [&str; 5],
    output_limit: usize,
) -> io::Result<bounded_command::Output> {
    let Some((program, prefix)) = argv.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no Secret Service client command",
        ));
    };
    let full: Vec<&OsStr> = prefix
        .iter()
        .copied()
        .chain(subcommand.iter().map(OsStr::new))
        .collect();
    bounded_command::run_os(program, &full, None, output_limit, Duration::from_secs(5))
        .map_err(io::Error::other)
}

fn secret_service_remove(locator: &str, argv: &[&OsStr]) -> io::Result<()> {
    validate_software_locator(locator)?;
    let mut removed = false;
    for (purpose_key, purpose_value, identity_key) in SECRET_ATTRIBUTES {
        let output = secret_tool(
            argv,
            ["clear", purpose_key, purpose_value, identity_key, locator],
            1024,
        )?;
        removed |= output.status.success();
    }
    if removed {
        Ok(())
    } else {
        Err(io::Error::other(
            "Secret Service refused or could not remove the identity key",
        ))
    }
}

// Attribute names are persistent storage ABI, independent of the display name.
const SECRET_ATTRIBUTES: [(&str, &str, &str); 2] = [
    ("tilo-purpose", PURPOSE_ATTRIBUTE, "tilo-identity-key"),
    ("qos-purpose", "qos-identity-signing-v1", "qos-identity-key"),
];

fn secret_service_load(locator: &str, argv: &[&OsStr]) -> io::Result<Zeroizing<Vec<u8>>> {
    validate_software_locator(locator)?;
    for (purpose_key, purpose_value, identity_key) in SECRET_ATTRIBUTES {
        let output = secret_tool(
            argv,
            ["lookup", purpose_key, purpose_value, identity_key, locator],
            usize::try_from(MAX_IDENTITY_SECRET_BYTES)
                .unwrap_or(usize::MAX)
                .saturating_mul(2)
                .saturating_add(2),
        )?;
        if !output.status.success() {
            continue;
        }
        // Corruption in the preferred entry must surface; never mask it by
        // falling through to an older entry or generating a replacement key.
        let encoded =
            Zeroizing::new(String::from_utf8(output.stdout).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid keyring value")
            })?);
        let decoded = hex::decode(encoded.trim())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid keyring value"))?;
        return Ok(Zeroizing::new(decoded));
    }
    Err(crate::secret_service_unavailable_error())
}

#[derive(Debug, Clone)]
pub struct BrokerFileProvider {
    root: PathBuf,
}

impl BrokerFileProvider {
    /// Construct the compatible host-file fallback below an identity store.
    ///
    /// Host components may use this explicitly in recovery and deterministic
    /// tests. Normal migration should call `secure_identity`, which prefers
    /// Secret Service and exposes the fallback choice in Settings.
    pub fn new(identity_dir: &Path) -> Self {
        Self {
            root: identity_dir.join("secrets"),
        }
    }

    fn path(&self, locator: &str) -> io::Result<PathBuf> {
        validate_software_locator(locator)?;
        Ok(self
            .root
            .join(format!("{}.key", locator.to_ascii_lowercase())))
    }

    fn ensure_root(&self) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let meta = fs::symlink_metadata(&self.root)?;
        if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity secrets directory must be a real directory",
            ));
        }
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity secrets directory is not owned by the current user",
            ));
        }
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
    }
}

impl SecretProvider for BrokerFileProvider {
    fn protection(&self) -> KeyProtection {
        KeyProtection::BrokerFile
    }

    fn backend_id(&self) -> &'static str {
        "broker-file-v1"
    }

    fn store(&self, locator: &str, secret: &[u8]) -> io::Result<()> {
        self.ensure_root()?;
        let path = self.path(locator)?;
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let current = self.load(locator)?;
                if current.as_slice() == secret {
                    return Ok(());
                }
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a different key already uses this identity locator",
                ));
            }
            Err(error) => return Err(error),
        };
        file.write_all(secret)?;
        file.sync_all()
    }

    fn load(&self, locator: &str) -> io::Result<Zeroizing<Vec<u8>>> {
        self.ensure_root()?;
        let path = self.path(locator)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.file_type().is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity key fallback must be a private regular file",
            ));
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(MAX_IDENTITY_SECRET_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_IDENTITY_SECRET_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity secret has an invalid size",
            ));
        }
        Ok(bytes)
    }

    fn remove(&self, locator: &str) -> io::Result<()> {
        self.ensure_root()?;
        let path = self.path(locator)?;
        let meta = fs::symlink_metadata(&path)?;
        if !meta.file_type().is_file()
            || meta.file_type().is_symlink()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to remove an untrusted identity key file",
            ));
        }
        fs::remove_file(path)
    }
}

pub(crate) fn provider_for<'a>(
    identity_dir: &'a Path,
    reference: &SecretReference,
) -> io::Result<Box<dyn SecretProvider + 'a>> {
    match reference.backend.as_str() {
        "secret-service-v1" => Ok(Box::new(SecretServiceProvider)),
        "broker-file-v1" => Ok(Box::new(BrokerFileProvider::new(identity_dir))),
        "tpm2-sealed-v1" => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this identity is TPM-sealed, but no TPM provider is installed",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown identity secret backend",
        )),
    }
}

pub(crate) fn preferred_provider(identity_dir: &Path) -> Box<dyn SecretProvider + '_> {
    #[cfg(test)]
    {
        Box::new(BrokerFileProvider::new(identity_dir))
    }
    #[cfg(not(test))]
    {
        // Presence alone is insufficient: a container can have secret-tool but no
        // unlocked collection/session bus. Callers try the provider and fall back
        // only on a real store failure.
        let secret_tool_exists = std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|dir| dir.join("secret-tool").is_file())
        });
        if secret_tool_exists {
            Box::new(SecretServiceProvider)
        } else {
            Box::new(BrokerFileProvider::new(identity_dir))
        }
    }
}

pub(crate) fn fallback_provider(identity_dir: &Path) -> BrokerFileProvider {
    BrokerFileProvider::new(identity_dir)
}

#[cfg(test)]
mod rename_compat_tests {
    use super::*;

    #[test]
    fn secret_service_load_finds_legacy_attributes_without_creating_keys() {
        let dir = tempfile::tempdir().unwrap();
        let command = dir.path().join("secret-tool-fixture");
        let command = tilo_test_env::script(
            &command,
            r#"#!/bin/sh
[ "$1" = lookup ] || exit 99
[ "$2" = qos-purpose ] || exit 1
[ "$3" = qos-identity-signing-v1 ] || exit 99
[ "$4" = qos-identity-key ] || exit 99
printf '010203\n'
"#,
        )
        .unwrap();
        let secret = secret_service_load(&"ab".repeat(32), &command.exec_argv()).unwrap();
        assert_eq!(secret.as_slice(), &[1, 2, 3]);
    }
}

#[cfg(test)]
mod removal_compat_tests {
    use super::*;

    #[test]
    fn secret_service_removes_legacy_entry() {
        let dir = tempfile::tempdir().unwrap();
        let command = dir.path().join("secret-tool-fixture");
        let command = tilo_test_env::script(
            &command,
            r#"#!/bin/sh
[ "$1" = clear ] || exit 99
[ "$2" = qos-purpose ] || exit 1
[ "$3" = qos-identity-signing-v1 ] || exit 99
[ "$4" = qos-identity-key ] || exit 99
exit 0
"#,
        )
        .unwrap();
        secret_service_remove(&"ab".repeat(32), &command.exec_argv()).unwrap();
    }

    #[test]
    fn corrupt_current_secret_is_not_masked_by_legacy_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let command = dir.path().join("secret-tool-fixture");
        let command = tilo_test_env::script(
            &command,
            r#"#!/bin/sh
[ "$1" = lookup ] || exit 99
if [ "$2" = tilo-purpose ]; then printf 'not-hex'; else printf '010203'; fi
"#,
        )
        .unwrap();
        assert_eq!(
            secret_service_load(&"ab".repeat(32), &command.exec_argv())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
