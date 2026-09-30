use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliancePhase {
    NotProvisioned,
    Installing { media_digest: String },
    AwaitingGuestBridge,
    Ready,
    RepairRequired { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseState {
    /// The guest has not yet reported its activation state. Launches fail closed.
    Unknown,
    DigitallyActivated,
    ProductKeyActivated,
    OrganisationManaged,
    Evaluation {
        expires_unix_seconds: u64,
    },
    Unlicensed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplianceState {
    pub schema_version: u32,
    pub phase: AppliancePhase,
    pub license: LicenseState,
    /// Monotonic catalogue generation last accepted from the authenticated guest bridge.
    pub catalog_generation: u64,
    /// Last complete, authenticated guest publication. This is a passive cache:
    /// reading it never wakes the singleton appliance. A missing or mismatched
    /// cache is deliberately treated as no published applications rather than
    /// as an invitation to run arbitrary Windows input.
    #[serde(default)]
    pub cached_catalog: Option<crate::Catalog>,
}

impl Default for ApplianceState {
    fn default() -> Self {
        Self {
            schema_version: 1,
            phase: AppliancePhase::NotProvisioned,
            license: LicenseState::Unknown,
            catalog_generation: 0,
            cached_catalog: None,
        }
    }
}

impl ApplianceState {
    #[must_use]
    pub fn can_launch(&self) -> bool {
        self.phase == AppliancePhase::Ready && self.license != LicenseState::Unknown
    }
}

#[derive(Clone, Debug)]
pub struct ProvisioningStore {
    path: PathBuf,
}

impl ProvisioningStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A missing state file is an unprovisioned appliance, never an implicit
    /// fully configured Windows installation.
    pub fn load(&self) -> Result<ApplianceState, ProvisioningError> {
        match fs::read(&self.path) {
            Ok(bytes) => {
                let state: ApplianceState = serde_json::from_slice(&bytes)?;
                if state.schema_version != 1 {
                    return Err(ProvisioningError::UnsupportedSchema(state.schema_version));
                }
                Ok(state)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(ApplianceState::default())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Persists state using a private temporary file and atomic rename. This
    /// crate intentionally keeps installation media and product keys out of
    /// the state document.
    pub fn save(&self, state: &ApplianceState) -> Result<(), ProvisioningError> {
        if state.schema_version != 1 {
            return Err(ProvisioningError::UnsupportedSchema(state.schema_version));
        }
        let parent = self.path.parent().ok_or(ProvisioningError::MissingParent)?;
        fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec_pretty(state)?;
        tilo_files::native::atomic::write_with_mode(&self.path, &bytes, 0o600)?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ProvisioningError {
    #[error("provisioning state has no parent directory")]
    MissingParent,
    #[error("unsupported provisioning-state schema {0}")]
    UnsupportedSchema(u32),
    #[error("could not access provisioning state: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid provisioning state: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn vm_state_round_trips_privately_and_defaults_to_unprovisioned() {
        // Phase `missing_file_is_not_provisioned`.
        {
            let directory = TempDir::new().unwrap();
            let store = ProvisioningStore::new(directory.path().join("state.json"));
            assert_eq!(store.load().unwrap(), ApplianceState::default());
            assert!(!store.load().unwrap().can_launch());
        }
        // Phase `state_round_trips_privately`.
        {
            let directory = TempDir::new().unwrap();
            let store = ProvisioningStore::new(directory.path().join("state.json"));
            let state = ApplianceState {
                schema_version: 1,
                phase: AppliancePhase::Ready,
                license: LicenseState::DigitallyActivated,
                catalog_generation: 19,
                cached_catalog: None,
            };
            store.save(&state).unwrap();
            assert_eq!(store.load().unwrap(), state);
            assert_eq!(fs::metadata(store.path()).unwrap().mode() & 0o777, 0o600);
        }
    }
}
