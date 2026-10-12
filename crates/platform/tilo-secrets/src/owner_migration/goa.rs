//! Exact GOA owner consent. Key schema and runtime registration are upstream:
//! <https://github.com/GNOME/gnome-online-accounts/blob/master/src/goabackend/goautils.c>
//! <https://github.com/GNOME/gnome-online-accounts/blob/master/data/dbus-interfaces.xml>
use crate::{Secret, SecretError};
use std::{fs::File, os::unix::fs::MetadataExt};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoaProvider {
    Google,
    Microsoft,
}
impl GoaProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Microsoft => "ms_graph",
        }
    }
    pub(crate) fn generation(self) -> u32 {
        // These generations are actual current upstream credential formats;
        // unknown/historical formats require reconnect, never wildcard reads.
        match self {
            Self::Google => 13,
            Self::Microsoft => 2,
        }
    }
}
/// Private original installed-app registration, never a UI projection.
pub struct GoaRegistration {
    pub client_id: String,
    pub client_secret: Zeroizing<String>,
}
impl std::fmt::Debug for GoaRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GoaRegistration { private original registration }")
    }
}
pub struct GoaCredentials {
    pub registration: Option<GoaRegistration>,
    pub secret: Secret,
}
impl std::fmt::Debug for GoaCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GoaCredentials { private selected credentials }")
    }
}
/// Retained selected original config grant; no arbitrary keychain search surface.
pub struct GoaCredential {
    directory: File,
    witness: (u32, u64, u64),
    provider: GoaProvider,
    id: String,
    identity: String,
}
impl GoaCredential {
    /// Construct only after the owner explicitly includes this discovered account.
    /// # Errors
    /// Refuses foreign/elevated callers, uncontrolled namespace, or unsafe account IDs.
    pub fn new(
        directory: &File,
        provider: GoaProvider,
        id: &str,
        identity: &str,
    ) -> Result<Self, SecretError> {
        let metadata = directory.metadata().map_err(|_| SecretError::Denied)?;
        let uid = unsafe { libc::getuid() };
        if uid == 0
            || unsafe { libc::geteuid() } != uid
            || !metadata.is_dir()
            || metadata.uid() != uid
            || metadata.mode() & 0o022 != 0
            || id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
            || identity.is_empty()
            || identity.len() > 256
            || identity.chars().any(char::is_control)
        {
            return Err(SecretError::Denied);
        }
        Ok(Self {
            directory: directory.try_clone().map_err(|_| SecretError::Denied)?,
            witness: (uid, metadata.dev(), metadata.ino()),
            provider,
            id: id.into(),
            identity: identity.into(),
        })
    }
    /// Read exact current source registration and existing refresh credentials only.
    /// # Errors
    /// Refuses changed custody, ambiguous/locked or unavailable credentials, or foreign runtime accounts.
    pub fn read_blocking(&self) -> Result<GoaCredentials, SecretError> {
        let facts = self.directory.metadata().map_err(|_| SecretError::Denied)?;
        if (facts.uid(), facts.dev(), facts.ino()) != self.witness
            || facts.mode() & 0o022 != 0
            || unsafe { libc::getuid() } != self.witness.0
            || unsafe { libc::geteuid() } != self.witness.0
        {
            return Err(SecretError::Denied);
        }
        crate::linux::read_goa(
            self.witness.0,
            self.provider,
            self.id.clone(),
            self.identity.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn goa_owner_grant_rejects_unbound_queries_before_any_bus_access() {
        let root = tempfile::tempdir().unwrap();
        let directory = File::open(root.path()).unwrap();
        for id in ["", "../other", "account/1", "account:1", "account\n1"] {
            assert!(
                GoaCredential::new(&directory, GoaProvider::Google, id, "person@example.test")
                    .is_err()
            );
        }
        let ordinary = GoaCredential::new(
            &directory,
            GoaProvider::Google,
            "account_1",
            "person@example.test",
        );
        if unsafe { libc::getuid() } == 0 {
            assert!(ordinary.is_err());
        } else {
            let grant = ordinary.unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
            assert!(matches!(grant.read_blocking(), Err(SecretError::Denied)));
        }
        let file = File::create(root.path().join("not-config")).unwrap();
        assert!(
            GoaCredential::new(
                &file,
                GoaProvider::Microsoft,
                "account_1",
                "person@example.test"
            )
            .is_err()
        );
    }
    #[test]
    fn goa_debug_never_exposes_registration_or_variant_credentials() {
        let credentials = GoaCredentials {
            registration: Some(GoaRegistration {
                client_id: "private-client-sentinel".into(),
                client_secret: Zeroizing::new("private-client-secret-sentinel".into()),
            }),
            secret: Secret::new(b"private-variant-token-sentinel".to_vec()),
        };
        let debug = format!("{:?} {:?}", credentials, credentials.registration);
        for secret in [
            "private-client-sentinel",
            "private-client-secret-sentinel",
            "private-variant-token-sentinel",
        ] {
            assert!(!debug.contains(secret));
        }
    }
}
