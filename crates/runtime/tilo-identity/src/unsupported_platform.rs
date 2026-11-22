//! Fail-closed identity enrollment for a platform without a local identity provider.
//!
//! Unix uses `tilo_identity::store`'s protected identity store. Windows
//! imager can prepare Wi-Fi and media, but cannot enroll a person until that
//! store has a Windows implementation. Keep this boundary in this crate so
//! the imager remains portable and never fabricates an identity.

#![allow(
    clippy::missing_errors_doc,
    clippy::unnecessary_wraps,
    clippy::unused_self,
    reason = "portable API stubs preserve the Unix signatures and always refuse enrollment"
)]

use std::io;
use std::path::{Path, PathBuf};

use tilo_device_identity::IdentityPaths;

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "local identity enrollment is unavailable on this machine",
    )
}

pub mod provisioning {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum IdentityStorage {
        Platform,
        Files,
    }

    pub fn collaboration_identity_paths() -> IdentityPaths {
        let root = std::env::var_os("LOCALAPPDATA")
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join("Tilo")
            .join("identity");
        IdentityPaths::under(root)
    }

    pub fn ensure_collaboration_identity(
        _identity_dir: &Path,
        _paths: &IdentityPaths,
        _storage: IdentityStorage,
    ) -> io::Result<()> {
        Err(unavailable())
    }
}

pub mod store {
    use super::*;

    pub struct IdentityRegistryStore {
        identity_dir: PathBuf,
    }

    impl IdentityRegistryStore {
        pub fn from_env() -> io::Result<Self> {
            Err(unavailable())
        }

        pub fn identity_dir(&self) -> &Path {
            &self.identity_dir
        }
    }
}
