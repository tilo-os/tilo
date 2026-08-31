//! Host-side storage for identity signing material.
//!
//! The preferred backend is the freedesktop Secret Service, reached through
//! the standard `secret-tool` client shipped by libsecret.  A session without
//! a Secret Service (a rescue shell, CI, or a non-GNOME/KDE desktop) falls back
//! to a mode-0600 file below the mode-0700 identity directory.  That fallback
//! is deliberately named *broker file*: the launcher may read it, sandboxed
//! applications never receive either its path or its bytes.
//!
//! `SecretProvider` is the hardware-backed seam.  A TPM/FIDO implementation
//! can implement it and pass it to the `_with_provider` APIs; this crate does
//! not claim hardware protection merely because `/dev/tpm*` exists.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use zeroize::Zeroizing;

use crate::{KeyProtection, SecretReference};

const PURPOSE_ATTRIBUTE: &str = "qos-identity-signing-v1";

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
        // secret-tool's stdin is text. Hex is unambiguous, contains no NULs,
        // and is decoded into a zeroizing buffer on lookup.
        let encoded = Zeroizing::new(hex::encode(secret));
        let mut child = Command::new("secret-tool")
            .args([
                "store",
                "--label=qOS identity signing key",
                "qos-purpose",
                PURPOSE_ATTRIBUTE,
                "qos-identity-key",
                locator,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("secret-tool stdin unavailable"))?
            .write_all(encoded.as_bytes())?;
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(
                "Secret Service refused or could not store the identity key",
            ))
        }
    }

    fn load(&self, locator: &str) -> io::Result<Zeroizing<Vec<u8>>> {
        validate_software_locator(locator)?;
        let output = Command::new("secret-tool")
            .args([
                "lookup",
                "qos-purpose",
                PURPOSE_ATTRIBUTE,
                "qos-identity-key",
                locator,
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "identity key is unavailable from Secret Service",
            ));
        }
        let encoded =
            Zeroizing::new(String::from_utf8(output.stdout).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid keyring value")
            })?);
        let decoded = hex::decode(encoded.trim())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid keyring value"))?;
        Ok(Zeroizing::new(decoded))
    }

    fn remove(&self, locator: &str) -> io::Result<()> {
        validate_software_locator(locator)?;
        let status = Command::new("secret-tool")
            .args([
                "clear",
                "qos-purpose",
                PURPOSE_ATTRIBUTE,
                "qos-identity-key",
                locator,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(
                "Secret Service refused or could not remove the identity key",
            ))
        }
    }
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
        let mut bytes = Zeroizing::new(Vec::with_capacity(32));
        file.take(33).read_to_end(&mut bytes)?;
        if bytes.len() != 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity key fallback has the wrong size",
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
