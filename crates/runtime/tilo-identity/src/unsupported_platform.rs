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
use tilo_user_identity::DeviceCert;

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

    pub struct JoinedDeviceLayout<'a> {
        pub target_root: &'a Path,
        pub username: &'a str,
        pub local_identity_dir: &'a Path,
        pub local_system: &'a IdentityPaths,
        pub local_home_peers: &'a Path,
        /// The setup machine's exported personal room list; the shell
        /// publishes it under that name.
        pub local_workspaces: &'a Path,
    }

    pub fn collaboration_identity_paths() -> IdentityPaths {
        let root = std::env::var_os("LOCALAPPDATA")
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join("Tilo")
            .join("identity");
        IdentityPaths::under(root)
    }

    pub fn personal_workspaces_export_path() -> Option<PathBuf> {
        None
    }

    pub fn ensure_collaboration_identity(
        _identity_dir: &Path,
        _paths: &IdentityPaths,
        _storage: IdentityStorage,
    ) -> io::Result<()> {
        Err(unavailable())
    }

    pub fn enroll_from_local_identity(
        _layout: &JoinedDeviceLayout<'_>,
        _display_name: &str,
        _device_label: &str,
        _now_ns: u64,
    ) -> io::Result<DeviceCert> {
        Err(unavailable())
    }
}

pub mod registry {
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
