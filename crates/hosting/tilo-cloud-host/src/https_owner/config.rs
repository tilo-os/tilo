//! Trusted operator choices, never remote first-claim authority or readiness.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OperatorConfig {
    pub(super) version: u16,
    pub(super) state_root: PathBuf,
    pub(super) machine_id: String,
    pub(super) transport_config: PathBuf,
    pub(super) initial_owner_root: Option<String>,
}
impl OperatorConfig {
    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= 16 * 1024,
            "Cloud operator config exceeds bound"
        );
        let config: Self = serde_json::from_slice(bytes)?;
        ensure!(config.version == 1, "Cloud operator version 1 required");
        ensure!(
            config.state_root.is_absolute() && config.transport_config.is_absolute(),
            "absolute Cloud deployment paths required"
        );
        ensure!(
            !config.machine_id.is_empty()
                && config.machine_id.len() <= 256
                && !config.machine_id.chars().any(char::is_control),
            "invalid configured machine identity"
        );
        config.pinned_root()?;
        Ok(config)
    }
    pub(super) fn load(path: &Path) -> Result<Self> {
        ensure!(path.is_absolute(), "absolute Cloud config path required");
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() <= 16 * 1024,
            "invalid bounded Cloud config file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            ensure!(
                metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.permissions().mode() & 0o077 == 0,
                "private operator configuration required"
            );
        }
        let mut bytes = Vec::new();
        file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
        Self::decode(&bytes)
    }
    fn pinned_root(&self) -> Result<Option<[u8; 32]>> {
        let Some(value) = &self.initial_owner_root else {
            return Ok(None);
        };
        ensure!(
            value.len() == 64,
            "initial_owner_root must be a public 32-byte hex key"
        );
        let mut key = [0; 32];
        hex::decode_to_slice(value, &mut key)?;
        ensure!(
            key != [0; 32] && hex::encode(key) == *value,
            "invalid canonical initial_owner_root"
        );
        Ok(Some(key))
    }
    pub(super) fn owner_root(&self, store: &crate::Store) -> Result<[u8; 32]> {
        store.ensure_reconciled()?;
        ensure!(
            store.state.machine_id == self.machine_id,
            "configured machine mismatch"
        );
        let configured = self.pinned_root()?;
        if let Some(bound) = &store.state.bound {
            ensure!(
                configured.is_none_or(|key| key == bound.root),
                "configured owner root mismatch"
            );
            Ok(bound.root)
        } else {
            configured.context("fresh HTTPS enrollment requires operator-pinned initial_owner_root")
        }
    }
}
#[cfg(test)]
mod hosted_attempt_tests;
#[cfg(test)]
mod tests;
